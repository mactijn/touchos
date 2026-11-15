//! The i.MX watchdog (i.MX21/25/27 "imx2-wdt"): restarts the chip when
//! nobody shows a sign of life for RESTART_SECS. init pets it while the
//! system is healthy (WATCHDOG syscall).
//!
//! Its enable bit (WDE) can only be set once per chip reset: once armed it
//! can't be stopped, so `stop` only logs. So it is armed as late as
//! possible: when init asks whether there is one (its QUERY at start-up),
//! never without init. `watchdog=off` and `panic=halt` (keep a panic on the
//! screen) leave it unarmed. It pauses while a debugger (JTAG) has the CPU
//! halted (WDBG), so a memory dump doesn't restart the board.
//!
//! The registers are mapped at boot either way: `reboot` uses them, and
//! must not take the paging locks (it runs on the panic path).

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::soc;

/// Time without a pet until the restart.
const RESTART_SECS: u16 = 60;

// 16-bit registers.
const WCR: usize = 0x00;
const WSR: usize = 0x02;
const WRSR: usize = 0x04;

const WCR_WDZST: u16 = 1 << 0; // pause in low-power modes
const WCR_WDBG: u16 = 1 << 1; // pause while the debugger has the CPU halted
const WCR_WDE: u16 = 1 << 2; // enable (write-once)
const WCR_SRS: u16 = 1 << 4; // 0 = software reset now
const WCR_WDA: u16 = 1 << 5; // 0 = assert the WDOG pin

/// The registers' kernel address (0: not mapped yet).
static BASE: AtomicUsize = AtomicUsize::new(0);
/// Arm it when init asks (not `watchdog=off` or `panic=halt`).
static WANTED: AtomicBool = AtomicBool::new(false);
/// Armed: from then on only a pet or a restart.
static ARMED: AtomicBool = AtomicBool::new(false);

fn w(base: usize, off: usize, v: u16) {
    unsafe { ((base + off) as *mut u16).write_volatile(v) }
}
fn r(base: usize, off: usize) -> u16 {
    unsafe { ((base + off) as *const u16).read_volatile() }
}

/// At boot: map the registers and report why the chip last reset. `off`
/// (with the reason) keeps it from being armed later.
pub fn init(off: Option<&str>) {
    let base = crate::mem::map_mmio(soc::WDOG, 0x10);
    BASE.store(base, Ordering::Relaxed);
    // WRSR: bit 0 software reset, 1 watchdog timeout, 3 external reset,
    // 4 power-on reset (also what JTAG's SRST gives on the Toon), 5 JTAG.
    let why = r(base, WRSR);
    let cause = match why {
        v if v & 0x02 != 0 => "the watchdog",
        v if v & 0x01 != 0 => "a software reset (restart)",
        v if v & 0x08 != 0 => "the external reset pin",
        v if v & 0x10 != 0 => "power-on (or JTAG's SRST)",
        v if v & 0x20 != 0 => "JTAG",
        _ => "unknown",
    };
    ::log::info!("watchdog: last reset by {cause} (WRSR {why:#06x})");
    match off {
        Some(why) => ::log::warn!("watchdog: off ({why})"),
        None => WANTED.store(true, Ordering::Relaxed),
    }
}

/// Arm it, once, if wanted.
fn arm() {
    let base = BASE.load(Ordering::Relaxed);
    if base == 0 || !WANTED.load(Ordering::Relaxed) || ARMED.swap(true, Ordering::Relaxed) {
        return;
    }
    // WT: (WT + 1) half seconds.
    let wt = RESTART_SECS * 2 - 1;
    let wcr = wt << 8 | WCR_WDA | WCR_SRS | WCR_WDBG | WCR_WDZST;
    w(base, WCR, wcr);
    w(base, WCR, wcr | WCR_WDE);
    pet();
    ::log::info!("watchdog: armed, restart after {RESTART_SECS} s without a sign of life");
}

/// Reload the countdown: 0x5555 then 0xaaaa.
pub fn pet() {
    let base = BASE.load(Ordering::Relaxed);
    if running() {
        w(base, WSR, 0x5555);
        w(base, WSR, 0xaaaa);
    }
}

/// Not possible once armed: say so (the restart comes RESTART_SECS after
/// the last pet).
pub fn stop() {
    if running() {
        ::log::warn!("watchdog: can't be stopped on this chip; restart in {RESTART_SECS} s unless petted");
    }
}

pub fn running() -> bool {
    ARMED.load(Ordering::Relaxed)
}

/// init's QUERY: arm it now (init is running and will pet it).
pub fn query() -> bool {
    arm();
    running()
}

/// Restart the chip now: WCR with WDE set and SRS clear (a software reset).
/// Takes no locks (the panic path); maps the registers only if that hasn't
/// happened yet (a panic before `init`).
pub fn reset_now() -> ! {
    let mut base = BASE.load(Ordering::Relaxed);
    if base == 0 {
        base = crate::mem::map_mmio(soc::WDOG, 0x10);
    }
    w(base, WCR, WCR_WDE);
    crate::arch::halt_forever()
}
