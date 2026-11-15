//! The KInfo page: kernel data every process can read (mapped read-only at
//! `KINFO_VA`), so reading the clock needs no syscall.

use core::sync::atomic::{AtomicU64, Ordering};
use touchos_abi::{ABI_VERSION, KINFO_MAGIC, KInfo};

use crate::{mem, time};

static FRAME: AtomicU64 = AtomicU64::new(0);

fn kinfo() -> Option<&'static KInfo> {
    match FRAME.load(Ordering::Relaxed) {
        0 => None,
        f => Some(unsafe { &*(mem::phys_to_virt(f) as *const KInfo) }),
    }
}

static FB: spin::Mutex<touchos_abi::FbInfo> = spin::Mutex::new(touchos_abi::FbInfo {
    width: 0,
    height: 0,
    pitch: 0,
    bpp: 0,
    r_shift: 0,
    g_shift: 0,
    b_shift: 0,
    _pad: [0; 3],
});

/// Before `init` (crate::run does it, from the board's Boot).
pub fn set_framebuffer(fb: touchos_abi::FbInfo) {
    *FB.lock() = fb;
}

/// After `time::init`.
pub fn init() {
    let f = mem::alloc_frame();
    let k = unsafe { &mut *(mem::phys_to_virt(f) as *mut KInfo) };
    k.magic = KINFO_MAGIC;
    k.version = ABI_VERSION;
    let c = crate::board::clock::kinfo_clock();
    k.counter_hz = c.hz;
    k.snap_count.store(c.boot_count, Ordering::Relaxed);
    k.snap_us.store(0, Ordering::Relaxed);
    // Even (no update in progress), and not 0: on ARMv5 an atomic load can
    // become a compare-and-swap with 0, which would write a read-only page.
    k.snap_seq.store(2, Ordering::Relaxed);
    k.us_mult = c.us_mult;
    k.us_shift = c.us_shift;
    k.counter_va = match c.user_counter {
        Some((_, off)) => (crate::uabi::layout::COUNTER_VA + off as u64) as u32,
        None => 0,
    };
    k.boot_unix.store(time::boot_unix(), Ordering::Relaxed);
    k.fb = *FB.lock();
    FRAME.store(f, Ordering::Relaxed);
}

pub fn frame() -> u64 {
    FRAME.load(Ordering::Relaxed)
}

/// Every timer tick.
pub fn tick(ticks: u64) {
    if let Some(k) = kinfo() {
        k.ticks.store(ticks, Ordering::Relaxed);
        k.boot_unix.store(time::boot_unix(), Ordering::Relaxed);
        if let Some((count, us)) = crate::board::clock::snapshot() {
            // A seqlock: odd while the pair changes.
            k.snap_seq.fetch_add(1, Ordering::Release);
            k.snap_count.store(count, Ordering::Relaxed);
            k.snap_us.store(us, Ordering::Relaxed);
            k.snap_seq.fetch_add(1, Ordering::Release);
        }
        // Processes read it through their own (aliasing) mapping.
        crate::arch::paging::sync_direct(frame(), core::mem::size_of::<touchos_abi::KInfo>());
    }
}
