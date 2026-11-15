//! EHCI host controller, as the ChipIdea core of the i.MX has it (the
//! Toon's USB host 2, QEMU imx25's USB2): the asynchronous schedule only,
//! so control and bulk transfers (no interrupt or isochronous endpoints),
//! and root-port devices only (no hubs). Polled, like the other drivers.
//!
//! Data structures (queue heads, transfer descriptors) live in DMA memory,
//! which is uncached on ARM, in 64-byte slots of a pool. Each endpoint has
//! a queue head (QH) in the async schedule's ring; transfers are queued on
//! it with Linux's "dummy tail" scheme: the QH always ends in an inactive
//! qTD; a new transfer fills that one, gets a fresh dummy behind it, and is
//! activated last, so the controller can run a queue while it grows.

use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};

use crate::desc::Endpoint;
use crate::{HostController, PortStatus, Result, SPEED_FULL, SPEED_HIGH, SPEED_LOW, UsbError};
use touchos_drv::mem::{self, Dma};
use touchos_drv::mmio::Regs;
use touchos_rt::{exec, time};

// Capability registers (from the controller's base).
const HCSPARAMS: usize = 0x04;
// Operational registers (from base + CAPLENGTH).
const USBCMD: usize = 0x00;
const USBSTS: usize = 0x04;
const USBINTR: usize = 0x08;
const ASYNCLISTADDR: usize = 0x18;
const CONFIGFLAG: usize = 0x40;
const PORTSC: usize = 0x44;
/// ChipIdea: host or device mode (3 = host); a controller reset clears it.
const USBMODE: usize = 0x68;
/// ChipIdea: the ULPI viewport (access to the PHY's registers).
const ULPIVIEW: usize = 0x30;
const ULPI_WU: u32 = 1 << 31;
const ULPI_RUN: u32 = 1 << 30;
const ULPI_RW: u32 = 1 << 29;
const ULPI_SS: u32 = 1 << 27;
/// ULPI PHY register: OTG Control.
const ULPI_OTG_CTRL: u8 = 0x0a;
/// OTG Control for a host port that powers its device, as stock Linux
/// leaves the Toon's OTG port: DrvVbus and DrvVbusExternal (the PHY
/// switches VBUS on), IdPullup, and the D+/D- pull-downs a host needs.
const ULPI_OTG_HOST_VBUS: u8 = 0x67;

const CMD_RS: u32 = 1 << 0;
const CMD_RESET: u32 = 1 << 1;
const CMD_ASE: u32 = 1 << 5;
const CMD_IAAD: u32 = 1 << 6;
const STS_IAA: u32 = 1 << 5;
/// USBSTS/USBINTR: a transfer with IOC completed (USBINT), one failed
/// (USBERRINT), a port changed (PCD).
const STS_EVENTS: u32 = 0b111;
const STS_HCH: u32 = 1 << 12;
const STS_ASS: u32 = 1 << 15;

const PORT_CCS: u32 = 1 << 0;
const PORT_CSC: u32 = 1 << 1;
const PORT_PE: u32 = 1 << 2;
const PORT_PEC: u32 = 1 << 3;
const PORT_OCC: u32 = 1 << 5;
const PORT_PR: u32 = 1 << 8;
const PORT_PP: u32 = 1 << 12;
const PORT_W1C: u32 = PORT_CSC | PORT_PEC | PORT_OCC;
/// ChipIdea: PHY low-power suspend (must be clear).
const PORT_PHCD: u32 = 1 << 23;
/// ChipIdea: the port's speed (0 full, 1 low, 2 high).
const PORT_PSPD_SHIFT: u32 = 26;
/// ChipIdea: the transceiver (PTS): ULPI.
const PORT_PTS_MASK: u32 = 3 << 30;
const PORT_PTS_ULPI: u32 = 2 << 30;

/// Link pointer: invalid (T) and "this is a QH" (Typ = 1).
const LINK_T: u32 = 1;
const LINK_QH: u32 = 1 << 1;

// qTD token bits.
const TOK_ACTIVE: u32 = 1 << 7;
const TOK_HALTED: u32 = 1 << 6;
const TOK_ERRORS: u32 = 0x7c; // halted, buffer error, babble, transaction error, missed µframe
const TOK_IOC: u32 = 1 << 15;
const TOK_CERR3: u32 = 3 << 10;
const PID_OUT: u32 = 0;
const PID_IN: u32 = 1;
const PID_SETUP: u32 = 2;

// QH dword offsets (bytes).
const QH_LINK: usize = 0;
const QH_INFO1: usize = 4;
const QH_INFO2: usize = 8;
const QH_CURRENT: usize = 12;
const QH_NEXT: usize = 16;
const QH_ALT: usize = 20;
const QH_TOKEN: usize = 24;
// qTD dword offsets (bytes).
const TD_NEXT: usize = 0;
const TD_ALT: usize = 4;
const TD_TOKEN: usize = 8;
const TD_BUF: usize = 12;

const CTRL_TIMEOUT_MS: u64 = 1000;
/// One qTD carries at most 5 pages; with an unaligned start, 16 KiB is safe.
pub const MAX_TRANSFER: u32 = 16 * 1024;

/// A 64-byte slot in DMA memory (QHs need 48 bytes, qTDs 32; both 32-byte
/// aligned).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Slot {
    phys: u32,
    virt: usize,
}

impl Slot {
    fn rd(self, off: usize) -> u32 {
        unsafe { ((self.virt + off) as *const u32).read_volatile() }
    }
    fn wr(self, off: usize, v: u32) {
        unsafe { ((self.virt + off) as *mut u32).write_volatile(v) }
    }
    fn zero(self) {
        for off in (0..64).step_by(4) {
            self.wr(off, 0);
        }
    }
}

/// QH and qTD slots, in DMA pages that are never freed.
#[derive(Default)]
struct Pool {
    pages: Vec<Dma>,
    free: Vec<Slot>,
}

impl Pool {
    fn alloc(&mut self) -> Slot {
        if self.free.is_empty() {
            let page = mem::alloc_dma(4096);
            for i in (0..4096).step_by(64).rev() {
                self.free.push(Slot { phys: (page.phys + i as u64) as u32, virt: page.virt as usize + i });
            }
            self.pages.push(page);
        }
        let s = self.free.pop().unwrap();
        s.zero();
        s
    }
    fn free(&mut self, s: Slot) {
        self.free.push(s);
    }
}

/// A queued transfer.
struct Td {
    slot: Slot,
}

/// An endpoint's queue head, its dummy tail and its queued transfers.
struct Qh {
    slot: Slot,
    dummy: Slot,
    pending: Vec<Td>,
    /// Finished qTDs the controller may still point at (QH current).
    retired: Vec<Slot>,
    halted: bool,
}

/// A device on the root port.
pub struct EhciDev {
    addr: u8,
    ep0: Qh,
    /// Bulk endpoints by index (`Endpoint::dci`).
    eps: Vec<Option<Qh>>,
}

pub struct Ehci {
    /// The operational registers.
    op: Regs,
    ports: u8,
    ulpi: bool,
    pool: Pool,
    /// The async schedule's head QH (never executes); then the endpoints'
    /// QHs, in ring order.
    head: Slot,
    ring: Vec<Slot>,
    /// The 8-byte SETUP packet of control transfers.
    setup: Dma,
    next_addr: u8,
}

fn spin_until(what: &str, ms: u64, cond: impl FnMut() -> bool) -> Result<()> {
    crate::spin_until("ehci", what, ms, cond)
}

/// Read a register of the ULPI PHY through the viewport (waking the PHY
/// up first if it is not in sync).
fn ulpi_read(op: Regs, reg: u8) -> Option<u8> {
    if op.r32(ULPIVIEW) & ULPI_SS == 0 {
        op.w32(ULPIVIEW, ULPI_WU);
        spin_until("ULPI wake-up", 10, || op.r32(ULPIVIEW) & ULPI_WU == 0).ok()?;
    }
    op.w32(ULPIVIEW, ULPI_RUN | (reg as u32) << 16);
    spin_until("ULPI read", 10, || op.r32(ULPIVIEW) & ULPI_RUN == 0).ok()?;
    Some((op.r32(ULPIVIEW) >> 8) as u8)
}

/// Write a register of the ULPI PHY through the viewport.
fn ulpi_write(op: Regs, reg: u8, val: u8) -> Option<()> {
    if op.r32(ULPIVIEW) & ULPI_SS == 0 {
        op.w32(ULPIVIEW, ULPI_WU);
        spin_until("ULPI wake-up", 10, || op.r32(ULPIVIEW) & ULPI_WU == 0).ok()?;
    }
    op.w32(ULPIVIEW, ULPI_RUN | ULPI_RW | (reg as u32) << 16 | val as u32);
    spin_until("ULPI write", 10, || op.r32(ULPIVIEW) & ULPI_RUN == 0).ok()
}

/// The error a finished qTD's token reports.
fn token_error(tok: u32) -> UsbError {
    if tok & 0x3c == 0 {
        UsbError::Stall // halted without another reason
    } else {
        UsbError::Completion(tok & TOK_ERRORS)
    }
}

impl Ehci {
    /// The controller in an i.MX USB block, as init hands it to a program:
    /// `regs` maps the block's page, and the program's arguments `args`
    /// say "ehci <controller offset> [ulpi] [vbus]". None (logged) if they name
    /// no controller or the page can't be mapped; else [`Ehci::init`]'s
    /// result.
    pub fn from_cap(regs: u32, args: &str) -> Option<Result<Ehci>> {
        let off = args
            .split_whitespace()
            .skip_while(|w| *w != "ehci")
            .nth(1)
            .and_then(|w| usize::from_str_radix(w.trim_start_matches("0x"), 16).ok());
        let (Some(off), Ok(page)) = (off, Regs::from_cap(regs, 0, 0x1000)) else {
            ::log::warn!("no EHCI controller (arguments \"{args}\")");
            return None;
        };
        // ChipIdea: the EHCI capability registers start 0x100 into the controller.
        let has = |w: &str| args.split_whitespace().any(|a| a == w);
        Some(Ehci::init(page.at(off + 0x100), has("ulpi"), has("vbus")))
    }

    /// Reset and start the controller whose capability registers are
    /// `base`. `ulpi`: the port has a ULPI PHY (the Toon); the board code
    /// has set up its pins and clocks. `vbus`: the PHY switches the port's
    /// VBUS (the Toon's external port); it is turned on here.
    pub fn init(base: Regs, ulpi: bool, vbus: bool) -> Result<Ehci> {
        let caplen = base.r32(0) & 0xff;
        let version = base.r32(0) >> 16;
        let ports = (base.r32(HCSPARAMS) & 0xf) as u8;
        let op = base.at(caplen as usize);

        op.w32(USBCMD, op.r32(USBCMD) & !CMD_RS);
        spin_until("controller halt", 20, || op.r32(USBSTS) & STS_HCH != 0)?;
        op.w32(USBCMD, CMD_RESET);
        spin_until("controller reset", 100, || op.r32(USBCMD) & CMD_RESET == 0)?;
        // The reset puts a ChipIdea core in device-less idle mode: host mode
        // again, and the PHY interface (as Linux's tdi_reset does).
        op.w32(USBMODE, 3);
        if ulpi {
            op.w32(PORTSC, (op.r32(PORTSC) & !PORT_PTS_MASK & !PORT_W1C) | PORT_PTS_ULPI);
        }
        op.w32(USBINTR, 0);

        let mut pool = Pool::default();
        // The head of the reclamation list: H = 1, an empty overlay that is
        // halted, so the controller never runs it; linked to itself.
        let head = pool.alloc();
        head.wr(QH_LINK, head.phys | LINK_QH);
        head.wr(QH_INFO1, 1 << 15);
        head.wr(QH_NEXT, LINK_T);
        head.wr(QH_ALT, LINK_T);
        head.wr(QH_TOKEN, TOK_HALTED);
        fence(Ordering::SeqCst);
        op.w32(ASYNCLISTADDR, head.phys);
        op.w32(USBCMD, CMD_RS | CMD_ASE | 8 << 16); // interrupt threshold 1 ms
        spin_until("controller start", 20, || op.r32(USBSTS) & STS_HCH == 0)?;
        op.w32(CONFIGFLAG, 1);
        // Port power (ports with power switches).
        for p in 1..=ports {
            let a = PORTSC + 4 * (p as usize - 1);
            op.w32(a, (op.r32(a) & !PORT_W1C & !PORT_PHCD) | PORT_PP);
        }
        if ulpi && vbus && ulpi_write(op, ULPI_OTG_CTRL, ULPI_OTG_HOST_VBUS).is_none() {
            ::log::warn!("ehci: VBUS on through the ULPI PHY failed");
        }
        let phy = if ulpi {
            let id = |r| ulpi_read(op, r).unwrap_or(0xff) as u16;
            let otg = if vbus { alloc::format!(", OTG control {:#04x}", id(ULPI_OTG_CTRL)) } else { alloc::string::String::new() };
            alloc::format!(", ULPI PHY {:04x}:{:04x}{otg}", id(0) | id(1) << 8, id(2) | id(3) << 8)
        } else {
            alloc::string::String::new()
        };
        ::log::info!(
            "ehci: v{:x}.{:02x}, {ports} port{}{phy}",
            version >> 8,
            version & 0xff,
            if ports == 1 { "" } else { "s" },
        );
        Ok(Ehci {
            op,
            ports,
            ulpi,
            pool,
            head,
            ring: Vec::new(),
            setup: mem::alloc_dma(64),
            next_addr: 1,
        })
    }

    fn portsc_off(&self, port: u8) -> usize {
        PORTSC + 4 * (port as usize - 1)
    }
    fn portsc(&self, port: u8) -> u32 {
        self.op.r32(self.portsc_off(port))
    }
    /// Write PORTSC, leaving the write-1-to-clear bits alone.
    fn set_portsc(&self, port: u8, f: impl FnOnce(u32) -> u32) {
        let v = self.portsc(port) & !PORT_W1C;
        self.op.w32(self.portsc_off(port), f(v));
    }

    /// A new QH for endpoint `ep` of device `addr`, with an inactive dummy
    /// qTD, linked into the async schedule.
    fn new_qh(&mut self, addr: u8, ep: u8, speed: u8, mps: u16, control: bool) -> Qh {
        let qh = self.pool.alloc();
        let dummy = self.pool.alloc();
        dummy.wr(TD_NEXT, LINK_T);
        dummy.wr(TD_ALT, LINK_T);
        let eps = match speed {
            SPEED_HIGH => 2,
            SPEED_LOW => 1,
            _ => 0,
        };
        let nak_reload = if speed == SPEED_HIGH { 4 } else { 0 };
        let info1 = addr as u32
            | ((ep & 0xf) as u32) << 8
            | eps << 12
            | (control as u32) << 14 // control: toggle from the qTD
            | ((mps & 0x7ff) as u32) << 16
            | ((control && speed != SPEED_HIGH) as u32) << 27
            | nak_reload << 28;
        qh.wr(QH_INFO1, info1);
        qh.wr(QH_INFO2, 1 << 30); // one transaction per µframe
        qh.wr(QH_NEXT, dummy.phys);
        qh.wr(QH_ALT, LINK_T);
        // Into the ring, right after the head.
        qh.wr(QH_LINK, self.head.rd(QH_LINK));
        fence(Ordering::SeqCst);
        self.head.wr(QH_LINK, qh.phys | LINK_QH);
        self.ring.insert(0, qh);
        Qh { slot: qh, dummy, pending: Vec::new(), retired: Vec::new(), halted: false }
    }

    /// Take QHs out of the schedule and wait until the controller no longer
    /// looks at them (the async advance doorbell); then free them.
    async fn remove_qhs(&mut self, qhs: Vec<Qh>) {
        if qhs.is_empty() {
            return;
        }
        for q in &qhs {
            if let Some(i) = self.ring.iter().position(|s| *s == q.slot) {
                let prev = if i == 0 { self.head } else { self.ring[i - 1] };
                prev.wr(QH_LINK, q.slot.rd(QH_LINK));
                self.ring.remove(i);
            }
        }
        fence(Ordering::SeqCst);
        self.async_advance().await;
        for q in qhs {
            for t in q.pending {
                self.pool.free(t.slot);
            }
            for s in q.retired {
                self.pool.free(s);
            }
            self.pool.free(q.dummy);
            self.pool.free(q.slot);
        }
    }

    /// Fill qTD `s` (inactive) for one transfer; `next`: the qTD after it.
    fn fill_td(s: Slot, pid: u32, toggle: bool, buf: u64, len: u32, next: u32) {
        s.wr(TD_NEXT, next);
        s.wr(TD_ALT, LINK_T);
        let first = buf as u32;
        s.wr(TD_BUF, first);
        for i in 1..5 {
            s.wr(TD_BUF + 4 * i, (first & !0xfff).wrapping_add(0x1000 * i as u32));
        }
        s.wr(TD_TOKEN, (toggle as u32) << 31 | len << 16 | TOK_CERR3 | pid << 8);
    }

    /// Make a filled qTD active (last, after everything else is written).
    fn activate(s: Slot) {
        fence(Ordering::SeqCst);
        s.wr(TD_TOKEN, s.rd(TD_TOKEN) | TOK_ACTIVE);
    }

    /// After an error: drop what is queued and restart the QH empty (data
    /// toggle 0) at its dummy.
    fn reset_qh(&mut self, q: &mut Qh) {
        for t in q.pending.drain(..) {
            t.slot.wr(TD_TOKEN, 0);
            q.retired.push(t.slot);
        }
        q.slot.wr(QH_NEXT, q.dummy.phys);
        q.slot.wr(QH_ALT, LINK_T);
        fence(Ordering::SeqCst);
        q.slot.wr(QH_TOKEN, 0); // clears halted and the toggle
        q.halted = false;
    }

    /// Cancel what a QH is still running (a control transfer that timed
    /// out: the device keeps NAKing): take it out of the schedule, wait
    /// until the controller has let go of it, reset it, link it back in.
    async fn restart_qh(&mut self, q: &mut Qh) {
        if let Some(i) = self.ring.iter().position(|s| *s == q.slot) {
            let prev = if i == 0 { self.head } else { self.ring[i - 1] };
            prev.wr(QH_LINK, q.slot.rd(QH_LINK));
            self.ring.remove(i);
            fence(Ordering::SeqCst);
            self.async_advance().await;
        }
        for t in q.pending.drain(..) {
            q.retired.push(t.slot);
        }
        q.halted = true;
        self.reset_qh(q);
        q.slot.wr(QH_LINK, self.head.rd(QH_LINK));
        fence(Ordering::SeqCst);
        self.head.wr(QH_LINK, q.slot.phys | LINK_QH);
        self.ring.insert(0, q.slot);
    }

    /// Wait until the controller has passed through the async schedule once
    /// (so it holds no pointer into what was just unlinked).
    async fn async_advance(&mut self) {
        if self.op.r32(USBSTS) & STS_ASS == 0 {
            return;
        }
        self.op.w32(USBSTS, STS_IAA);
        self.op.w32(USBCMD, self.op.r32(USBCMD) | CMD_IAAD);
        let end = time::now_ms() + 20;
        while self.op.r32(USBSTS) & STS_IAA == 0 && time::now_ms() < end {
            exec::yield_now().await;
        }
        self.op.w32(USBSTS, STS_IAA);
    }

    /// Free retired qTDs the controller no longer points at.
    fn reap(&mut self, q: &mut Qh) {
        let current = q.slot.rd(QH_CURRENT);
        let pool = &mut self.pool;
        q.retired.retain(|s| {
            if s.phys == current {
                true
            } else {
                pool.free(*s);
                false
            }
        });
    }

    /// A control transfer on a device's EP0 QH, waited for.
    async fn control_qh(&mut self, q: &mut Qh, setup: [u8; 8], buf: &mut Dma) -> Result<usize> {
        if q.halted {
            self.reset_qh(q);
        }
        self.reap(q);
        let dir_in = setup[0] & 0x80 != 0;
        let len = u16::from_le_bytes([setup[6], setup[7]]) as u32;
        if len > MAX_TRANSFER {
            return Err(UsbError::Protocol("control transfer too long"));
        }
        self.setup.bytes_mut()[..8].copy_from_slice(&setup);
        // SETUP (DATA0) in the current dummy, then DATA (DATA1), STATUS
        // (DATA1, the other direction), and a new dummy.
        let st = q.dummy;
        let data = (len > 0).then(|| self.pool.alloc());
        let status = self.pool.alloc();
        let dummy = self.pool.alloc();
        dummy.wr(TD_NEXT, LINK_T);
        dummy.wr(TD_ALT, LINK_T);
        Self::fill_td(status, if len > 0 && dir_in { PID_OUT } else { PID_IN }, true, 0, 0, dummy.phys);
        status.wr(TD_TOKEN, status.rd(TD_TOKEN) | TOK_IOC);
        if let Some(d) = data {
            Self::fill_td(d, if dir_in { PID_IN } else { PID_OUT }, true, buf.phys, len, status.phys);
        }
        Self::fill_td(st, PID_SETUP, false, self.setup.phys, 8, data.map_or(status.phys, |d| d.phys));
        Self::activate(status);
        if let Some(d) = data {
            Self::activate(d);
        }
        Self::activate(st);
        q.dummy = dummy;

        let tds: Vec<Slot> = [Some(st), data, Some(status)].into_iter().flatten().collect();
        let end = time::now_ms() + CTRL_TIMEOUT_MS;
        let result = loop {
            // The clock before the look (see touchos_drv::mmio::poll_until).
            let late = time::now_ms() > end;
            // Done when the status stage is, or when any stage stopped with
            // an error (then the rest never runs).
            if let Some(t) = tds.iter().map(|s| s.rd(TD_TOKEN)).find(|t| t & TOK_HALTED != 0) {
                break Err(token_error(t));
            }
            if status.rd(TD_TOKEN) & TOK_ACTIVE == 0 {
                let got = data.map_or(0, |d| len - ((d.rd(TD_TOKEN) >> 16) & 0x7fff));
                break Ok(got as usize);
            }
            if late {
                break Err(UsbError::Timeout);
            }
            exec::yield_now().await;
        };
        q.retired.extend(tds);
        match result {
            // Still running: unlink it to stop it safely.
            Err(UsbError::Timeout) => self.restart_qh(q).await,
            // Halted: the controller skips it; reset it in place.
            Err(_) => {
                q.halted = true;
                self.reset_qh(q);
            }
            Ok(_) => {}
        }
        result
    }
}

impl HostController for Ehci {
    type Dev = EhciDev;

    fn ports(&self) -> u8 {
        self.ports
    }

    fn enable_interrupts(&mut self) {
        self.op.w32(USBINTR, STS_EVENTS);
    }

    fn clear_interrupts(&mut self) {
        self.op.w32(USBSTS, STS_EVENTS);
    }

    fn poll(&mut self) -> Vec<u8> {
        (1..=self.ports).filter(|&p| self.portsc(p) & (PORT_CSC | PORT_PEC) != 0).collect()
    }

    fn port_status(&self, port: u8) -> PortStatus {
        let v = self.portsc(port);
        PortStatus { connected: v & PORT_CCS != 0, enabled: v & PORT_PE != 0 }
    }

    fn ack_port(&mut self, port: u8) {
        let v = self.portsc(port);
        self.op.w32(self.portsc_off(port), (v & !PORT_W1C) | (v & PORT_W1C));
    }

    async fn reset_port(&mut self, port: u8, force: bool) -> Result<u8> {
        if force || self.portsc(port) & PORT_PE == 0 {
            // PR for 50 ms (the root-port reset time), then end it.
            self.set_portsc(port, |v| (v & !PORT_PE) | PORT_PR);
            exec::sleep_ms(50).await;
            self.set_portsc(port, |v| v & !PORT_PR);
            let end = time::now_ms() + 50;
            loop {
                let late = time::now_ms() > end; // (before the look)
                if self.portsc(port) & PORT_PR == 0 {
                    break;
                }
                if late {
                    return Err(UsbError::Protocol("port reset did not complete"));
                }
                exec::sleep_ms(1).await;
            }
            exec::sleep_ms(if force { 100 } else { 20 }).await; // reset recovery
        }
        let v = self.portsc(port);
        if v & PORT_PE == 0 {
            // A full- or low-speed device on a controller without a
            // transaction translator; the ChipIdea core has one.
            return Err(UsbError::Protocol("port not enabled after reset"));
        }
        if self.ulpi && v & PORT_PTS_MASK != PORT_PTS_ULPI {
            ::log::warn!("ehci: port {port}: PTS is not ULPI ({v:#010x})");
        }
        // The ChipIdea core (the Toon) reports the speed in PSPD, and also
        // enables full- and low-speed ports (it has a transaction
        // translator). A plain EHCI (QEMU's, where PSPD reads 0) only
        // enables high-speed devices.
        if !self.ulpi {
            return Ok(SPEED_HIGH);
        }
        Ok(match (v >> PORT_PSPD_SHIFT) & 3 {
            2 => SPEED_HIGH,
            1 => SPEED_LOW,
            _ => SPEED_FULL,
        })
    }

    /// Read the first 8 bytes of the device descriptor at address 0 (for
    /// EP0's packet size), then SET_ADDRESS, then a new EP0 QH at the address.
    async fn address(&mut self, port: u8, speed: u8, buf: &mut Dma) -> Result<EhciDev> {
        let mps0 = if speed == SPEED_LOW { 8 } else { 64 };
        let mut ep0 = self.new_qh(0, 0, speed, mps0, true);
        let r = async {
            buf.bytes_mut()[..8].fill(0);
            self.control_qh(&mut ep0, [0x80, 6, 0, 1, 0, 0, 8, 0], buf).await?;
            let mps0 = match buf.bytes()[7] {
                m @ (8 | 16 | 32 | 64) => m as u16,
                _ => mps0,
            };
            let addr = self.next_addr;
            self.control_qh(&mut ep0, [0x00, 5, addr, 0, 0, 0, 0, 0], buf).await?;
            Ok::<_, UsbError>((addr, mps0))
        }
        .await;
        self.remove_qhs(alloc::vec![ep0]).await;
        let (addr, mps0) = r.inspect_err(|e| ::log::warn!("port {port}: addressing failed: {e}"))?;
        exec::sleep_ms(10).await; // SET_ADDRESS recovery (spec: 2 ms)
        self.next_addr = if addr >= 127 { 1 } else { addr + 1 };
        let ep0 = self.new_qh(addr, 0, speed, mps0, true);
        Ok(EhciDev { addr, ep0, eps: (0..32).map(|_| None).collect() })
    }

    async fn control(&mut self, d: &mut EhciDev, setup: [u8; 8], buf: &mut Dma) -> Result<usize> {
        self.control_qh(&mut d.ep0, setup, buf).await
    }

    async fn configure_endpoints(&mut self, d: &mut EhciDev, eps: &[Endpoint]) -> Result<()> {
        let speed = match (d.ep0.slot.rd(QH_INFO1) >> 12) & 3 {
            2 => SPEED_HIGH,
            1 => SPEED_LOW,
            _ => SPEED_FULL,
        };
        for ep in eps {
            if ep.transfer_type() != 2 {
                return Err(UsbError::Protocol("only bulk endpoints (no periodic schedule)"));
            }
        }
        for ep in eps {
            if let Some(old) = d.eps[ep.dci() as usize].take() {
                self.remove_qhs(alloc::vec![old]).await;
            }
            let q = self.new_qh(d.addr, ep.number(), speed, ep.max_packet & 0x7ff, false);
            d.eps[ep.dci() as usize] = Some(q);
        }
        Ok(())
    }

    /// The token is the qTD's address.
    fn queue(&mut self, d: &mut EhciDev, ep: u8, buf_phys: u64, len: u32) -> u64 {
        let i = ((ep & 0xf) * 2 + (ep >> 7)) as usize;
        let q = d.eps[i].as_mut().expect("endpoint not configured");
        if q.halted {
            self.reset_qh(q);
        }
        self.reap(q);
        let t = q.dummy;
        let dummy = self.pool.alloc();
        dummy.wr(TD_NEXT, LINK_T);
        dummy.wr(TD_ALT, LINK_T);
        let pid = if ep & 0x80 != 0 { PID_IN } else { PID_OUT };
        Self::fill_td(t, pid, false, buf_phys, len.min(MAX_TRANSFER), dummy.phys);
        t.wr(TD_TOKEN, t.rd(TD_TOKEN) | TOK_IOC);
        Self::activate(t);
        q.dummy = dummy;
        q.pending.push(Td { slot: t });
        t.phys as u64
    }

    fn check(&mut self, d: &mut EhciDev, ep: u8, token: u64) -> Option<Result<u32>> {
        let i = ((ep & 0xf) * 2 + (ep >> 7)) as usize;
        let q = d.eps[i].as_mut()?;
        let k = q.pending.iter().position(|t| t.slot.phys as u64 == token)?;
        let tok = q.pending[k].slot.rd(TD_TOKEN);
        let r = if tok & TOK_HALTED != 0 || q.halted {
            q.halted = true;
            Err(if tok & TOK_HALTED != 0 { token_error(tok) } else { UsbError::Protocol("endpoint halted") })
        } else if tok & TOK_ACTIVE != 0 {
            return None;
        } else {
            Ok((tok >> 16) & 0x7fff)
        };
        let t = q.pending.remove(k);
        q.retired.push(t.slot);
        Some(r)
    }

    async fn disable(&mut self, d: EhciDev) {
        let mut qhs = alloc::vec![d.ep0];
        qhs.extend(d.eps.into_iter().flatten());
        self.remove_qhs(qhs).await;
    }
}
