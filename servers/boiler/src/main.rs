//! The Toon's boiler link: UART2 (`ttymxc1`, 0x1000b000, IRQ 19), the
//! line to the Ketelmodule, which speaks OpenTherm to the boiler. What the
//! module says on this line, and at what speed, isn't known yet: this
//! server is the groundwork to find out.
//!
//! **Read-only.** The UART's transmitter is never enabled and its TX pin
//! isn't touched (U-Boot leaves PE6/PE7 on UART2): nothing is ever sent to
//! the module, so nothing reaches the boiler.
//!
//! It records what arrives: bytes and line errors per baud rate, the first
//! bytes at a rate in hex, and, if the bytes are text lines in the
//! OpenTherm Gateway's format (`B40192B00`: a source letter and a frame in
//! hex), the frames decoded (libs/opentherm). A rate that gives mostly
//! framing errors is left for the next one. A summary every 10 minutes
//! while anything arrives.
//!
//! Capabilities: "uart" (UART2's page), "irq" (its interrupt).
#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use log::{info, warn};
use touchos_drv::mmio::Regs;
use touchos_opentherm::Frame;
use touchos_rt::irq::Irq;
use touchos_rt::{cap, exec, time};

touchos_rt::entry!(main);

const URXD: usize = 0x00;
const UCR1: usize = 0x80;
const UCR2: usize = 0x84;
const UCR3: usize = 0x88;
const UCR4: usize = 0x8c;
const UFCR: usize = 0x90;
const USR2: usize = 0x98;
const UBIR: usize = 0xa4;
const UBMR: usize = 0xa8;

const UCR1_UARTEN: u32 = 1 << 0;
/// Interrupt when the receive FIFO reaches its threshold.
const UCR1_RRDYEN: u32 = 1 << 9;
/// Not in reset, receiver on, 8 bits, ignore RTS. TXEN (bit 2) stays 0.
const UCR2_RX_ONLY: u32 = 1 << 0 | 1 << 1 | 1 << 5 | 1 << 14;
const USR2_RDR: u32 = 1 << 0;
const URXD_ERR: u32 = 1 << 14;

/// The rates tried, most likely first: 4800 is what stock `happ_thermstat`
/// opens `ttymxc1` at (`qmf_hardware.xml`). UART1 (the console) runs at
/// 115200 with UBIR 15, UBMR 102 and the same reference clock (UFCR's
/// divider as U-Boot set it): other rates follow from that.
const RATES: [u32; 8] = [4800, 9600, 19200, 38400, 57600, 115200, 2400, 1200];

/// A rate is left after this many characters, if most had errors.
const JUDGE_AFTER: u32 = 64;
const SUMMARY_EVERY_MS: u64 = 10 * 60_000;

struct Uart {
    regs: Regs,
}

impl Uart {
    fn new(regs: Regs) -> Uart {
        // As UART1: UCR3 0x704, UCR4 0x8000, the reference divider by 2
        // (UFCR 0x200), receive threshold 1 character.
        regs.w32(UCR2, 0); // reset
        regs.w32(UCR3, 0x704);
        regs.w32(UCR4, 0x8000);
        regs.w32(UFCR, 0x200 | 1);
        Uart { regs }
    }

    /// Out of reset first: the rate registers written while the UART is
    /// in reset are cleared when it leaves it (seen on the Toon). UBMR is
    /// written last: that loads the new rate.
    fn set_rate(&self, baud: u32) {
        let ubmr = 103 * 115_200 / baud - 1;
        self.regs.w32(UCR2, UCR2_RX_ONLY);
        self.regs.w32(UCR1, UCR1_UARTEN);
        self.regs.w32(UBIR, 15);
        self.regs.w32(UBMR, ubmr);
        self.regs.w32(UCR1, UCR1_UARTEN | UCR1_RRDYEN);
    }

    /// Everything in the receive FIFO: (byte, had an error).
    fn drain(&self, out: &mut Vec<(u8, bool)>) {
        while self.regs.r32(USR2) & USR2_RDR != 0 {
            let w = self.regs.r32(URXD);
            out.push((w as u8, w & URXD_ERR != 0));
        }
    }
}

#[derive(Default)]
struct Stats {
    chars: u32,
    errors: u32,
    frames: u32,
    shown: usize,
}

/// Lines of the OpenTherm Gateway's text format: a source letter (T
/// thermostat, B boiler, R/A the gateway) and 8 hex digits.
fn otgw_frame(line: &str) -> Option<(char, Frame)> {
    let line = line.trim();
    let (src, hex) = line.split_at_checked(1)?;
    let src = src.chars().next()?;
    if !matches!(src, 'T' | 'B' | 'R' | 'A') || hex.len() != 8 {
        return None;
    }
    let w = u32::from_str_radix(hex, 16).ok()?;
    Frame::parse(w).ok().map(|f| (src, f))
}

async fn listen(uart: Uart, irq: Option<Irq>) {
    let mut rate = 0;
    uart.set_rate(RATES[rate]);
    info!("UART2 receive-only at {} baud (nothing is ever sent)", RATES[rate]);
    let mut st = Stats::default();
    let mut total = 0u64;
    let mut line = String::new();
    let mut buf = Vec::new();
    let mut next_summary = time::now_ms() + SUMMARY_EVERY_MS;
    loop {
        match &irq {
            Some(i) => {
                i.ack();
                exec::wait_bits_or_ms(i.bit(), 60_000).await;
            }
            None => exec::sleep_ms(100).await,
        }
        buf.clear();
        uart.drain(&mut buf);
        for &(b, err) in &buf {
            st.chars += 1;
            total += 1;
            if err {
                st.errors += 1;
            }
            if st.shown < 64 {
                if st.shown == 0 {
                    info!("the first bytes at {} baud:", RATES[rate]);
                }
                st.shown += 1;
            }
            match b {
                b'\r' | b'\n' => {
                    if let Some((src, f)) = otgw_frame(&line) {
                        st.frames += 1;
                        match (f.data_id(), f.reading()) {
                            (Some(id), Some(r)) => info!("{src} {:?} {}: {r:?} {}", f.msg, id.what(), id.unit()),
                            _ => info!("{src} {:?} data ID {} value {:#06x}", f.msg, f.id, f.value),
                        }
                    }
                    line.clear();
                }
                b if b.is_ascii_graphic() && line.len() < 64 => line.push(b as char),
                _ => line.clear(),
            }
        }
        if !buf.is_empty() && st.shown > 0 && st.shown <= 64 {
            let hex: Vec<String> = buf.iter().take(64).map(|(b, e)| alloc::format!("{b:02x}{}", if *e { "!" } else { "" })).collect();
            info!("  {}", hex.join(" "));
        }
        // Mostly errors: probably the wrong speed.
        if st.chars >= JUDGE_AFTER && st.errors * 2 > st.chars && st.frames == 0 {
            warn!("{} of {} characters with errors at {} baud; trying {}", st.errors, st.chars, RATES[rate], RATES[(rate + 1) % RATES.len()]);
            rate = (rate + 1) % RATES.len();
            uart.set_rate(RATES[rate]);
            st = Stats::default();
        }
        if time::now_ms() >= next_summary {
            next_summary = time::now_ms() + SUMMARY_EVERY_MS;
            if total > 0 {
                info!("{total} bytes so far; at {} baud {} characters, {} errors, {} OpenTherm frames", RATES[rate], st.chars, st.errors, st.frames);
            }
        }
    }
}

fn main() {
    let Some(regs) = cap("uart").and_then(|c| Regs::from_cap(c, 0, 0x1000).ok()) else {
        warn!("no UART (\"uart\")");
        return;
    };
    let irq = Irq::take("irq");
    exec::spawn(listen(Uart::new(regs), irq));
    exec::run()
}
