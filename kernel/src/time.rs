//! Time: the board's monotonic clock, timer ticks and the wall clock.

use core::sync::atomic::{AtomicU64, Ordering};

pub use crate::board::clock::{is_calibrated, now_us};

/// How user code reads the clock (KInfo).
pub struct ClockInfo {
    pub hz: u64,
    /// The counter's value at boot (time 0).
    pub boot_count: u64,
    /// For 32-bit counters: `us = delta * us_mult >> us_shift`.
    pub us_mult: u32,
    pub us_shift: u32,
    /// A counter register user code reads through a read-only mapping:
    /// (physical page, offset in it). None: user code has its own (TSC).
    pub user_counter: Option<(u64, u32)>,
}

/// Timer interrupts since boot (counted by the timer interrupt handler).
pub static TICKS: AtomicU64 = AtomicU64::new(0);

/// The scheduler tick's rate: the GPT's compare period is derived from it.
pub const TICK_HZ: u64 = 250;

/// The timer interrupt (interrupts off), from the architecture's or the
/// board's handler. `from_user`: it interrupted user code.
pub fn tick(from_user: bool) {
    // Every core has a timer; the boot core's counts.
    if crate::arch::cpu() == 0 {
        TICKS.fetch_add(1, Ordering::Relaxed);
    }
    crate::task::tick(from_user);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn now_ms() -> u64 {
    now_us() / 1000
}

pub fn busy_wait_us(us: u64) {
    let end = now_us() + us;
    while now_us() < end {
        core::hint::spin_loop();
    }
}

// ---- wall clock ----

static BOOT_UNIX: AtomicU64 = AtomicU64::new(0);

/// UNIX time at boot (0 = unknown).
pub fn boot_unix() -> u64 {
    BOOT_UNIX.load(Ordering::Relaxed)
}

/// Set the clock (user space does: the RTC, NTP).
pub fn set_unix_now(ts: u64) {
    BOOT_UNIX.store(ts.saturating_sub(now_ms() / 1000), Ordering::Relaxed);
}
