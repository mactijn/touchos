//! The clock and the scheduler tick: GPT1 counting the 32.768 kHz clock
//! (the same crystal on every board, so nothing to calibrate). Its 32-bit
//! counter wraps every 36 hours; it is extended to 64 bits here. The
//! compare interrupt comes every `PERIOD` counts (4 ms, the tick).
//!
//! i.MX27 GPTs have the older register layout (TCTL...), the i.MX25's the
//! newer one (GPTCR...): `soc::GPT_V2`.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use spin::Mutex;

use super::{irq, soc};
use crate::arch;

pub const HZ: u64 = 32768;
/// The tick: 32768 / 131 = 250.1 Hz.
const PERIOD: u32 = (HZ / crate::time::TICK_HZ) as u32;
const _: () = assert!(PERIOD == 131);

// Older layout (i.MX1/21/27).
const TCTL: usize = 0x00;
const TPRER: usize = 0x04;
const TCMP: usize = 0x08;
const TCN: usize = 0x10;
const TSTAT: usize = 0x14;
// Newer layout (i.MX25/31/35).
const GPTCR: usize = 0x00;
const GPTPR: usize = 0x04;
const GPTSR: usize = 0x08;
const GPTIR: usize = 0x0c;
const GPTOCR1: usize = 0x10;
const GPTCNT: usize = 0x24;

static BASE: AtomicUsize = AtomicUsize::new(0);
/// GPT2 on the fast peripheral clock (only for jitter).
static FAST: AtomicUsize = AtomicUsize::new(0);
static READY: AtomicBool = AtomicBool::new(false);
/// (last counter value read, wraps so far).
static EXT: Mutex<(u32, u32)> = Mutex::new((0, 0));

fn reg(off: usize) -> *mut u32 {
    (BASE.load(Ordering::Relaxed) + off) as *mut u32
}

fn count() -> u32 {
    unsafe { reg(if soc::GPT_V2 { GPTCNT } else { TCN }).read_volatile() }
}

fn compare_reg() -> *mut u32 {
    reg(if soc::GPT_V2 { GPTOCR1 } else { TCMP })
}

pub fn init() {
    let b = crate::mem::map_mmio(soc::GPT, 0x100);
    BASE.store(b, Ordering::Relaxed);
    unsafe {
        if soc::GPT_V2 {
            reg(GPTCR).write_volatile(0);
            reg(GPTCR).write_volatile(1 << 15); // software reset
            while reg(GPTCR).read_volatile() & 1 << 15 != 0 {}
            reg(GPTPR).write_volatile(0);
            reg(GPTSR).write_volatile(0x3f);
            // 32 kHz clock, free-running, counter reset when enabled.
            reg(GPTCR).write_volatile(0b100 << 6 | 1 << 9 | 1 << 1);
            reg(GPTOCR1).write_volatile(PERIOD);
            reg(GPTIR).write_volatile(1); // output compare 1
            reg(GPTCR).write_volatile(reg(GPTCR).read_volatile() | 1);
        } else {
            reg(TCTL).write_volatile(0);
            reg(TCTL).write_volatile(1 << 15); // software reset
            while reg(TCTL).read_volatile() & 1 << 15 != 0 {}
            reg(TPRER).write_volatile(0);
            reg(TSTAT).write_volatile(0x3);
            // 32 kHz clock, compare interrupt, free-running.
            reg(TCTL).write_volatile(0b100 << 1 | 1 << 4 | 1 << 8);
            reg(TCMP).write_volatile(PERIOD);
            reg(TCTL).write_volatile(reg(TCTL).read_volatile() | 1);
        }
    }
    READY.store(true, Ordering::Relaxed);
    start_fast_counter();
    seed_entropy();
    irq::enable(soc::IRQ_GPT);
    ::log::info!("clock: GPT at {:#x} on the 32 kHz clock, tick every {PERIOD} counts", soc::GPT);
}

/// GPT2 counting the peripheral clock (via the PLL), against the 32 kHz
/// one: their relative jitter is noise. Weak on the Toon: its PLL runs from
/// the 32 kHz crystal too (CSCR 0x43f0020f: MCU_SEL = 0, the 26 MHz
/// oscillator off), so only PLL phase noise is left; the
/// SAHARA RNG (`rng`) makes up for it.
fn start_fast_counter() {
    let b = crate::mem::map_mmio(soc::GPT_FAST, 0x100);
    FAST.store(b, Ordering::Relaxed);
    let r = |off: usize| (b + off) as *mut u32;
    unsafe {
        if soc::GPT_V2 {
            r(GPTCR).write_volatile(0);
            r(GPTPR).write_volatile(0);
            r(GPTCR).write_volatile(0b001 << 6 | 1 << 9 | 1 << 1 | 1); // ipg clock, free-running
        } else {
            r(TCTL).write_volatile(0);
            r(TPRER).write_volatile(0);
            r(TCTL).write_volatile(0b001 << 1 | 1 << 8 | 1); // PERCLK1, free-running
        }
    }
}

fn fast_count() -> u32 {
    let b = FAST.load(Ordering::Relaxed);
    unsafe { ((b + if soc::GPT_V2 { GPTCNT } else { TCN }) as *const u32).read_volatile() }
}

/// At boot: for 2048 edges of the 32 kHz clock (about 60 ms), how far the
/// fast counter and a busy loop got; plus the hardware RNG if there is one (`rng`).
fn seed_entropy() {
    let mut buf = [0u8; 64];
    let mut n = 0;
    // How much there is to it: the spread of the spin counts, and whether
    // the fast counter runs at all (its clock may be gated).
    let (mut lo, mut hi) = (u32::MAX, 0u32);
    let fast0 = fast_count();
    for _ in 0..2048 {
        let start = count();
        let mut spins = 0u32;
        while count() == start {
            spins = spins.wrapping_add(1);
        }
        lo = lo.min(spins);
        hi = hi.max(spins);
        buf[n..n + 4].copy_from_slice(&(fast_count() ^ spins.rotate_left(16)).to_le_bytes());
        n += 4;
        if n == buf.len() {
            crate::entropy::add(&buf);
            n = 0;
        }
    }
    let source = super::rng::seed();
    let fast = fast_count().wrapping_sub(fast0);
    ::log::info!(
        "entropy: {} samples at boot (clock jitter{source}): spins per 32 kHz edge {lo}..{hi}, fast counter +{fast}",
        crate::entropy::samples(),
    );
}

/// The counter extended to 64 bits.
fn extended() -> u64 {
    arch::irq::without(|| {
        let c = count();
        let mut e = EXT.lock();
        if c < e.0 {
            e.1 += 1;
        }
        e.0 = c;
        (e.1 as u64) << 32 | c as u64
    })
}

pub fn is_calibrated() -> bool {
    READY.load(Ordering::Relaxed)
}

/// Microseconds since the clock started: count * 10^6 / 2^15 = count * 15625 / 2^9.
pub fn now_us() -> u64 {
    if !is_calibrated() {
        return 0;
    }
    extended() * 15625 >> 9
}

/// For KInfo: user code reads the GPT's counter register (mapped
/// read-only) and converts with multiply-shift.
pub fn kinfo_clock() -> crate::time::ClockInfo {
    let (page, off) = (soc::GPT & !0xfff, (soc::GPT & 0xfff) as u32);
    crate::time::ClockInfo {
        hz: HZ,
        boot_count: 0,
        us_mult: 15625,
        us_shift: 9,
        user_counter: Some((page, off + if soc::GPT_V2 { GPTCNT } else { TCN } as u32)),
    }
}

/// A fresh (counter, microseconds) snapshot for KInfo: the 32-bit counter
/// wraps, so user code must not count too far from it.
pub fn snapshot() -> Option<(u64, u64)> {
    let e = extended();
    Some((e & 0xffff_ffff, e * 15625 >> 9))
}

/// The compare interrupt: next tick, then the scheduler.
pub fn interrupt(from_user: bool) {
    unsafe {
        if soc::GPT_V2 {
            reg(GPTSR).write_volatile(1);
        } else {
            reg(TSTAT).write_volatile(0x1);
        }
        // The next tick one period after the last; but if interrupts were
        // off for longer than that, the counter is already past it and the
        // compare would only match after a wrap (36 hours): then one period
        // from now.
        let c = compare_reg();
        let mut next = c.read_volatile().wrapping_add(PERIOD);
        let now = count();
        if (next.wrapping_sub(now) as i32) < 2 {
            next = now.wrapping_add(PERIOD);
        }
        c.write_volatile(next);
    }
    extended(); // notice wraps in time
    // Interrupt latency against the fast clock: a little noise per tick.
    crate::entropy::add(&fast_count().to_le_bytes());
    crate::time::tick(from_user);
}
