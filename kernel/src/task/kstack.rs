//! Kernel stacks for threads: 32 KiB each, in their own virtual window with
//! an unmapped guard page below every stack, so an overflow faults instead
//! of corrupting memory. The report comes from the data-abort entry
//! (`arm_kstack_overflow`).
//!
//! A freed stack stays mapped, with its frames, for the next thread: kernel
//! mappings are never removed, so no other core can hold a stale TLB entry
//! for one (no TLB shootdown needed).

use alloc::vec::Vec;
use spin::Mutex;

use crate::mem::{self, PAGE};

const WINDOW: u64 = crate::arch::paging::KSTACK_WINDOW;
const PAGES: u64 = 8;
const SLOT: u64 = (PAGES + 1) * PAGE;

struct Slots {
    next: u64,
    /// Slots whose stack is still mapped, ready for reuse.
    free: Vec<u64>,
}

static SLOTS: Mutex<Slots> = Mutex::new(Slots {
    next: 0,
    free: Vec::new(),
});

pub struct KStack {
    base: u64,
    /// Initial stack pointer (16-byte aligned).
    pub top: u64,
}

impl KStack {
    pub fn new() -> KStack {
        let (slot, mapped) = {
            let mut s = SLOTS.lock();
            match s.free.pop() {
                Some(slot) => (slot, true),
                None => {
                    s.next += 1;
                    (s.next - 1, false)
                }
            }
        };
        let base = WINDOW + slot * SLOT + PAGE; // the first page is the guard
        if !mapped {
            let frames: Vec<u64> = (0..PAGES).map(|_| mem::alloc_frame()).collect();
            mem::map_kernel(base, &frames);
        }
        KStack {
            base,
            top: base + PAGES * PAGE,
        }
    }
}

impl KStack {
    /// The lowest stack address (the guard page is just below).
    // test_fail=kstack
    pub fn bottom(&self) -> u64 {
        self.base
    }
}

impl Drop for KStack {
    fn drop(&mut self) {
        SLOTS.lock().free.push((self.base - PAGE - WINDOW) / SLOT);
    }
}
