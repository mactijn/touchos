//! The process heap: starts at `HEAP_BASE` and grows (contiguously) with
//! `VM_ALLOC` whenever an allocation doesn't fit.

use core::alloc::{GlobalAlloc, Layout};
use core::ptr::{NonNull, null_mut};
use linked_list_allocator::Heap;
use touchos_abi::{layout::HEAP_BASE, prot};

use crate::sys;

const PAGE: u64 = 4096;
const INITIAL: u64 = 256 * 1024;
const MIN_GROW: u64 = 1024 * 1024;

struct Growing {
    heap: Heap,
    /// End of the mapped heap region.
    end: u64,
}

/// One thread per process and no interrupts in user space: a simple flag
/// lock is enough (it only guards against re-entrance from a bug).
struct Lock(
    core::cell::UnsafeCell<Growing>,
    core::sync::atomic::AtomicBool,
);
unsafe impl Sync for Lock {}

static HEAP: Lock = Lock(
    core::cell::UnsafeCell::new(Growing {
        heap: Heap::empty(),
        end: HEAP_BASE as u64,
    }),
    core::sync::atomic::AtomicBool::new(false),
);

impl Lock {
    fn with<R>(&self, f: impl FnOnce(&mut Growing) -> R) -> R {
        use core::sync::atomic::Ordering;
        assert!(
            !self.1.swap(true, Ordering::Acquire),
            "heap re-entered (allocation inside the allocator?)"
        );
        let r = f(unsafe { &mut *self.0.get() });
        self.1.store(false, Ordering::Release);
        r
    }
}

pub fn init() {
    HEAP.with(|g| {
        if sys::vm_alloc(HEAP_BASE as u64, INITIAL, prot::WRITE).is_ok() {
            unsafe { g.heap.init(HEAP_BASE as *mut u8, INITIAL as usize) };
            g.end = HEAP_BASE as u64 + INITIAL;
        } else {
            ::log::error!("heap: VM_ALLOC failed; no heap");
        }
    });
}

impl Growing {
    fn grow(&mut self, need: usize) -> bool {
        let len = ((need as u64 + 2 * PAGE).max(MIN_GROW) + PAGE - 1) & !(PAGE - 1);
        if sys::vm_alloc(self.end, len, prot::WRITE).is_err() {
            return false;
        }
        unsafe { self.heap.extend(len as usize) };
        self.end += len;
        true
    }
}

struct Allocator;

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        HEAP.with(|g| {
            if let Ok(p) = g.heap.allocate_first_fit(layout) {
                return p.as_ptr();
            }
            if g.heap.size() == 0 || !g.grow(layout.size() + layout.align()) {
                return null_mut();
            }
            g.heap
                .allocate_first_fit(layout)
                .map_or(null_mut(), |p| p.as_ptr())
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        HEAP.with(|g| unsafe { g.heap.deallocate(NonNull::new_unchecked(ptr), layout) })
    }
}

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
