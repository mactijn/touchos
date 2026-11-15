//! The Fast Ethernet Controller of the i.MX25 and i.MX27 (the same design),
//! in user space, for the net server: frames in and out through the link
//! rings (`touchos_drv::link`). It wakes for a received frame (the FEC's
//! interrupt, RXF), a frame to send (the link's bit), and twice a second
//! to look at the PHY's link; without an interrupt, it polls every 4 ms.
//!
//! Capabilities: "regs" (the FEC's registers), "irq" (its interrupt, on
//! the i.MX boards), "link_rx", "link_tx", "link_state". The MAC address is what U-Boot programmed (the Toon's
//! own); without one, a random locally administered address.
//!
//! DMA: buffer descriptors and buffers in contiguous memory, which the
//! kernel maps uncached on ARM (the FEC doesn't see the CPU's caches).
#![no_std]
#![no_main]

extern crate alloc;

use log::{info, warn};
use touchos_drv::link;
use touchos_drv::mem::{Dma, alloc_dma};
use touchos_drv::mmio::{Regs, poll_until};
use touchos_rt::irq::Irq;
use touchos_rt::{cap, exec, time};

touchos_rt::entry!(main);

// Registers.
const EIR: usize = 0x004;
const EIMR: usize = 0x008;
const RDAR: usize = 0x010;
const TDAR: usize = 0x014;
const ECR: usize = 0x024;
const MMFR: usize = 0x040;
const MSCR: usize = 0x044;
const RCR: usize = 0x084;
const TCR: usize = 0x0c4;
const PALR: usize = 0x0e4;
const PAUR: usize = 0x0e8;
const IAUR: usize = 0x118;
const IALR: usize = 0x11c;
const GAUR: usize = 0x120;
const GALR: usize = 0x124;
const ERDSR: usize = 0x180;
const ETDSR: usize = 0x184;
const EMRBR: usize = 0x188;

const EIR_MII: u32 = 1 << 23;
/// A frame was received (the only interrupt we enable).
const EIR_RXF: u32 = 1 << 25;
const DESC_ACTIVE: u32 = 1 << 24;

// Buffer descriptors: length (16 bits), status (16), buffer address (32).
const BD_E_R: u32 = 0x8000; // rx: empty (ours to fill); tx: ready (the FEC's)
const BD_W: u32 = 0x2000; // wrap: the last one
const BD_L: u32 = 0x0800; // last in frame
const BD_TC: u32 = 0x0400; // tx: append the CRC
const RX_ERRORS: u32 = 0x0037; // LG, NO, CR, OV, TR

const RX_N: usize = 16;
const TX_N: usize = 8;
const BUF: usize = 1536; // a multiple of 16, as EMRBR wants

// PHY registers (IEEE 802.3 clause 22).
const BMCR: u8 = 0;
const BMSR: u8 = 1;
const PHYID1: u8 = 2;
const ANAR: u8 = 4;
const ANLPAR: u8 = 5;

struct Fec {
    regs: Regs,
    rx_bd: Dma,
    tx_bd: Dma,
    rx_buf: Dma,
    tx_buf: Dma,
    rx_i: usize,
    tx_i: usize,
    mac: [u8; 6],
    phy: Option<u8>,
}

impl Fec {
    fn mdio(&self, cmd: u32) -> Option<u16> {
        self.regs.w32(EIR, EIR_MII);
        self.regs.w32(MMFR, cmd);
        self.regs.wait32(EIR, EIR_MII, EIR_MII, 10_000).ok()?;
        self.regs.w32(EIR, EIR_MII);
        Some(self.regs.r32(MMFR) as u16)
    }
    fn mdio_read(&self, phy: u8, reg: u8) -> Option<u16> {
        self.mdio(0x6002_0000 | (phy as u32) << 23 | (reg as u32) << 18)
    }
    fn mdio_write(&self, phy: u8, reg: u8, v: u16) {
        let _ = self.mdio(0x5002_0000 | (phy as u32) << 23 | (reg as u32) << 18 | v as u32);
    }

    fn new(regs: Regs) -> Fec {
        let mut f = Fec {
            regs,
            rx_bd: alloc_dma(RX_N * 8),
            tx_bd: alloc_dma(TX_N * 8),
            rx_buf: alloc_dma(RX_N * BUF),
            tx_buf: alloc_dma(TX_N * BUF),
            rx_i: 0,
            tx_i: 0,
            mac: [0; 6],
            phy: None,
        };
        // The address U-Boot set, before the reset clears it.
        let (lo, hi) = (f.regs.r32(PALR), f.regs.r32(PAUR));
        f.mac = [(lo >> 24) as u8, (lo >> 16) as u8, (lo >> 8) as u8, lo as u8, (hi >> 24) as u8, (hi >> 16) as u8];
        if f.mac == [0; 6] || f.mac[0] & 1 != 0 {
            touchos_rt::rand::fill(&mut f.mac);
            f.mac[0] = (f.mac[0] & 0xfe) | 0x02; // unicast, locally administered
        }
        f.reset();
        f
    }

    fn reset(&mut self) {
        self.regs.w32(ECR, 1);
        let _ = self.regs.wait32(ECR, 1, 0, 10_000);
        self.regs.w32(EIMR, 0); // until `start`
        self.regs.w32(EIR, 0xffff_ffff);
        self.regs.w32(MSCR, 27 << 1); // MDC = 133 MHz (i.MX27 AHB) / 54: under 2.5 MHz
        let m = self.mac;
        self.regs.w32(PALR, u32::from_be_bytes([m[0], m[1], m[2], m[3]]));
        self.regs.w32(PAUR, (m[4] as u32) << 24 | (m[5] as u32) << 16 | 0x8808);
        for r in [IAUR, IALR, GAUR, GALR] {
            self.regs.w32(r, 0);
        }
        self.regs.w32(RCR, 1518 << 16 | 1 << 2); // max frame, MII mode
        self.regs.w32(TCR, 0);
        self.regs.w32(EMRBR, BUF as u32);
    }

    /// Fresh descriptor rings, all receive buffers empty, both indices at the
    /// start. Only while ETHER_EN is clear: setting it makes the FEC start
    /// over at ERDSR/ETDSR, so ours must too.
    fn rings(&mut self) {
        for i in 0..RX_N {
            let wrap = if i == RX_N - 1 { BD_W } else { 0 };
            self.rx_bd.write32(i * 8, (BD_E_R | wrap) << 16);
            self.rx_bd.write32(i * 8 + 4, (self.rx_buf.phys + (i * BUF) as u64) as u32);
        }
        for i in 0..TX_N {
            let wrap = if i == TX_N - 1 { BD_W } else { 0 };
            self.tx_bd.write32(i * 8, wrap << 16);
            self.tx_bd.write32(i * 8 + 4, (self.tx_buf.phys + (i * BUF) as u64) as u32);
        }
        self.regs.w32(ERDSR, self.rx_bd.phys as u32);
        self.regs.w32(ETDSR, self.tx_bd.phys as u32);
        self.rx_i = 0;
        self.tx_i = 0;
    }

    /// On link up: (re)start with fresh rings and the negotiated duplex.
    fn start(&mut self, full_duplex: bool) {
        self.regs.w32(ECR, 0);
        self.rings();
        self.regs.w32(TCR, if full_duplex { 1 << 2 } else { 0 });
        self.regs.w32(ECR, 2); // ETHER_EN
        self.regs.w32(EIR, EIR_RXF);
        self.regs.w32(EIMR, EIR_RXF);
        self.regs.w32(RDAR, DESC_ACTIVE);
    }

    /// On link down: stop (what was queued is dropped; `start` begins anew).
    fn stop(&self) {
        self.regs.w32(EIMR, 0);
        self.regs.w32(ECR, 0);
    }

    /// The PHY: the first address that answers (it may need a moment after
    /// its reset); reset it, then autonegotiate.
    fn find_phy(&mut self) {
        for attempt in 0..10 {
            self.phy = (0..32u8).find(|&a| matches!(self.mdio_read(a, PHYID1), Some(id) if id != 0 && id != 0xffff));
            if self.phy.is_some() || attempt == 9 {
                break;
            }
            time::busy_wait_us(100_000);
        }
        match self.phy {
            Some(p) => {
                let id = (self.mdio_read(p, PHYID1).unwrap_or(0) as u32) << 16 | self.mdio_read(p, 3).unwrap_or(0) as u32;
                info!("PHY at MDIO address {p}, id {id:#010x}");
                self.mdio_write(p, BMCR, 0x8000);
                // Until the reset bit clears (or the PHY stops answering).
                let _ = poll_until(time::now_us, 500_000, || {
                    (!self.mdio_read(p, BMCR).is_some_and(|v| v & 0x8000 != 0)).then_some(())
                });
                self.mdio_write(p, BMCR, 0x1200); // autonegotiation on, restart
            }
            None => warn!("no PHY answers on MDIO; assuming a link"),
        }
    }

    /// (link up, full duplex).
    fn link_state(&self) -> (bool, bool) {
        let Some(p) = self.phy else { return (true, true) };
        let _ = self.mdio_read(p, BMSR); // the link bit latches low: read twice
        let up = self.mdio_read(p, BMSR).is_some_and(|v| v & 0x4 != 0);
        let common = self.mdio_read(p, ANAR).unwrap_or(0) & self.mdio_read(p, ANLPAR).unwrap_or(0);
        let full = if common & 0x0180 != 0 { common & 0x0100 != 0 } else { common & 0x0040 != 0 };
        (up, full)
    }

    /// Hand received frames to net.
    fn receive(&mut self) -> usize {
        let mut n = 0;
        loop {
            let w = self.rx_bd.read32(self.rx_i * 8);
            let status = w >> 16;
            if status & BD_E_R != 0 {
                return n;
            }
            let len = (w & 0xffff) as usize;
            if status & BD_L != 0 && status & RX_ERRORS == 0 && len > 4 && len <= BUF {
                let off = self.rx_i * BUF;
                link::deliver(self.rx_buf.bytes()[off..off + len - 4].to_vec()); // without the CRC
                n += 1;
            }
            let wrap = if self.rx_i == RX_N - 1 { BD_W } else { 0 };
            self.rx_bd.write32(self.rx_i * 8, (BD_E_R | wrap) << 16);
            self.regs.w32(RDAR, DESC_ACTIVE);
            self.rx_i = (self.rx_i + 1) % RX_N;
        }
    }

    /// Send what net queued, while there are free descriptors.
    fn transmit(&mut self) -> usize {
        let mut n = 0;
        while self.tx_bd.read32(self.tx_i * 8) >> 16 & BD_E_R == 0 {
            let Some(frame) = link::pop_tx() else { break };
            let len = frame.len().clamp(60, BUF); // pad runts
            let off = self.tx_i * BUF;
            self.tx_buf.bytes_mut()[off..off + frame.len().min(BUF)].copy_from_slice(&frame[..frame.len().min(BUF)]);
            if frame.len() < 60 {
                self.tx_buf.bytes_mut()[off + frame.len()..off + 60].fill(0);
            }
            let wrap = if self.tx_i == TX_N - 1 { BD_W } else { 0 };
            self.tx_bd.write32(self.tx_i * 8, (BD_E_R | BD_L | BD_TC | wrap) << 16 | len as u32);
            self.regs.w32(TDAR, DESC_ACTIVE);
            self.tx_i = (self.tx_i + 1) % TX_N;
            n += 1;
        }
        n
    }
}

async fn run(regs: Regs) {
    let mut f = Fec::new(regs);
    let m = f.mac;
    info!("FEC, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5]);
    f.find_phy();
    let tx_bit = exec::alloc_bit();
    link::wait_tx(tx_bit);
    let irq = Irq::take("irq");
    let irq_bit = irq.as_ref().map_or(0, Irq::bit);
    let mut up = false;
    let mut next_check = 0;
    let mut fired = false;
    loop {
        if time::now_ms() >= next_check {
            next_check = time::now_ms() + 500;
            let (now_up, full) = f.link_state();
            if now_up && !up {
                info!("link up, {} duplex", if full { "full" } else { "half" });
                f.start(full);
                link::up(f.mac);
            } else if !now_up && up {
                info!("link down");
                f.stop();
                link::down();
            }
            up = now_up;
        }
        if fired {
            // Clear the event before taking the frames: one that arrives
            // after we looked sets it again and the line fires once more.
            f.regs.w32(EIR, EIR_RXF);
        }
        if up {
            f.receive();
            f.transmit();
        }
        if let Some(i) = irq.as_ref().filter(|_| fired) {
            i.ack();
        }
        // Without the interrupt, received frames are only seen by polling.
        let wait = match (&irq, up) {
            (None, true) => 4,
            _ => next_check.saturating_sub(time::now_ms()).max(1),
        };
        fired = exec::wait_bits_or_ms(tx_bit | irq_bit, wait).await & irq_bit != 0;
    }
}

fn main() {
    let regs = cap("regs").expect("no FEC registers (\"regs\")");
    let regs = Regs::from_cap(regs, 0, 0x400).expect("mmio_map");
    exec::spawn(run(regs));
    exec::run()
}
