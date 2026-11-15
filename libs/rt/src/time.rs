//! Time without syscalls: the kernel's KInfo page (mapped read-only in every
//! process) has the clock counter's rate, a snapshot and the wall-clock
//! offset (see `touchos_abi::KInfo`).

use core::sync::atomic::Ordering;
use touchos_abi::{KINFO_MAGIC, KInfo, layout::KINFO_VA};

/// The kernel's information page.
pub fn kinfo() -> &'static KInfo {
    let k = unsafe { &*(KINFO_VA as *const KInfo) };
    debug_assert_eq!(k.magic, KINFO_MAGIC);
    k
}

/// The snapshot (counter value, microseconds since boot), read consistently
/// (a seqlock; the kernel only writes it between our instructions, on one
/// CPU, so plain loads in program order suffice). Relaxed loads: on ARMv5
/// an Acquire load is a compare-and-swap, which may not touch this
/// read-only page.
fn snapshot(k: &KInfo) -> (u64, u64) {
    use core::sync::atomic::compiler_fence;
    loop {
        let seq = k.snap_seq.load(Ordering::Relaxed);
        compiler_fence(Ordering::SeqCst);
        if seq & 1 != 0 {
            core::hint::spin_loop();
            continue;
        }
        let snap = (k.snap_count.load(Ordering::Relaxed), k.snap_us.load(Ordering::Relaxed));
        compiler_fence(Ordering::SeqCst);
        if k.snap_seq.load(Ordering::Relaxed) == seq {
            return snap;
        }
    }
}

/// Microseconds since boot: a 32-bit counter register, mapped read-only,
/// and multiply-shift (the ARM926 has no divider).
pub fn now_us() -> u64 {
    let k = kinfo();
    let (count, us) = snapshot(k);
    let now = unsafe { core::ptr::read_volatile(k.counter_va as usize as *const u32) };
    let delta = now.wrapping_sub(count as u32) as u64;
    us + (delta * k.us_mult as u64 >> k.us_shift)
}

pub fn now_ms() -> u64 {
    now_us() / 1000
}

/// UNIX time in seconds (0 if the clock isn't set).
pub fn unix_now() -> u64 {
    match kinfo().boot_unix.load(Ordering::Relaxed) {
        0 => 0,
        b => b + now_ms() / 1000,
    }
}

/// Spin for `us` microseconds without giving up the CPU (for hardware delays).
pub fn busy_wait_us(us: u64) {
    let end = now_us() + us;
    while now_us() < end {
        core::hint::spin_loop();
    }
}

/// Sleep at least `ms` milliseconds (the process blocks; resolution is one
/// timer tick, 4 ms).
pub fn sleep_ms(ms: u64) {
    let deadline = now_us() + ms * 1000;
    while now_us() < deadline {
        let _ = crate::sys::wait(deadline);
    }
}
