//! Physical memory and the kernel heap.
//!
//! - All RAM is reachable through the kernel's direct map:
//!   `phys_to_virt(p) = p + direct_map_offset()`.
//! - Physical frames come from a bump allocator over the usable regions.
//!   Single frames can be freed (process memory, page tables, kernel stacks):
//!   they go on a free list that `alloc_frame` uses first. Contiguous runs
//!   (DMA buffers) come from the bump allocator and are never freed.
//! - The heap (kernel objects only) is carved out of the largest usable
//!   region.
//! - Page tables and kernel/MMIO mappings are the architecture's
//!   (`arch::paging`); its functions are re-exported here.

use core::sync::atomic::{AtomicU64, Ordering};
use linked_list_allocator::LockedHeap;
use spin::Mutex;

pub use crate::arch::paging::{map_kernel, map_mmio};

pub const PAGE: u64 = 4096;

/// How a page is cached (each architecture's paging turns this into its
/// page-table bits).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemKind {
    Normal,
    /// Device registers: uncached and unbuffered.
    Device,
    /// A framebuffer: uncached, but writes may be buffered.
    WriteCombining,
}

/// Access rights and caching of a page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attr {
    pub user: bool,
    pub write: bool,
    /// (ARMv5 has no execute-never bit: there readable means executable.)
    pub exec: bool,
    pub kind: MemKind,
}
/// Kernel objects only (processes get their memory as frames).
const MAX_HEAP: u64 = crate::board::KERNEL_HEAP_MAX;

/// One range of the machine's memory map.
#[derive(Clone, Copy)]
pub struct MemRegion {
    pub base: u64,
    pub len: u64,
    pub kind: RegionKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    /// Free RAM: frames and the heap come from here.
    Usable,
    /// RAM in use (the kernel image, boot modules, bootloader data): never
    /// allocated, but still RAM, so device mappings may not cover it.
    Ram,
}

#[global_allocator]
static HEAP: LockedHeap = LockedHeap::empty();

static DIRECT_MAP: AtomicU64 = AtomicU64::new(0);
static HEAP_READY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn heap_ready() -> bool {
    HEAP_READY.load(Ordering::Relaxed)
}

#[derive(Clone, Copy)]
struct Region {
    next: u64,
    end: u64,
}

struct Frames {
    regions: [Region; 128],
    count: usize,
    free_bytes: u64,
    /// Head of the list of freed frames (physical address; 0 = empty). Each
    /// free frame stores the next one in its first 8 bytes.
    free_list: u64,
    free_list_len: u64,
}

static FRAMES: Mutex<Frames> = Mutex::new(Frames {
    regions: [Region { next: 0, end: 0 }; 128],
    count: 0,
    free_bytes: 0,
    free_list: 0,
    free_list_len: 0,
});

/// Usable RAM ranges from the memory map: device mappings must never cover them.
static RAM: Mutex<([(u64, u64); 128], usize)> = Mutex::new(([(0, 0); 128], 0));

/// Does `[phys, phys+len)` touch usable RAM?
pub fn overlaps_ram(phys: u64, len: u64) -> bool {
    let r = RAM.lock();
    let end = phys.saturating_add(len);
    r.0[..r.1].iter().any(|&(b, l)| phys < b + l && b < end)
}

/// The virtual address of physical address 0 in the direct map.
pub fn direct_map_offset() -> u64 {
    DIRECT_MAP.load(Ordering::Relaxed)
}

pub fn phys_to_virt(phys: u64) -> *mut u8 {
    (phys + direct_map_offset()) as *mut u8
}

/// The inverse of `phys_to_virt`.
pub fn virt_to_phys(virt: *const u8) -> u64 {
    virt as u64 - direct_map_offset()
}

pub fn init(direct_map: u64, regions: &[MemRegion]) {
    DIRECT_MAP.store(direct_map, Ordering::Relaxed);

    let mut frames = FRAMES.lock();
    // Largest usable region hosts the heap.
    let largest = regions
        .iter()
        .filter(|e| e.kind == RegionKind::Usable)
        .max_by_key(|e| e.len)
        .expect("no usable memory");
    let heap_size = (largest.len / 2).min(MAX_HEAP) & !(PAGE - 1);
    let heap_phys = largest.base;

    {
        let mut ram = RAM.lock();
        for e in regions {
            if ram.1 < 128 {
                let n = ram.1;
                ram.0[n] = (e.base, e.len);
                ram.1 += 1;
            }
        }
    }
    for e in regions.iter().filter(|e| e.kind == RegionKind::Usable) {
        let mut start = (e.base + PAGE - 1) & !(PAGE - 1);
        let end = (e.base + e.len) & !(PAGE - 1);
        if e.base == heap_phys {
            start += heap_size;
        }
        // Keep the first MiB free of allocations (firmware leftovers, tiny regions).
        if end <= 0x10_0000 || start >= end || frames.count == frames.regions.len() {
            continue;
        }
        let n = frames.count;
        frames.regions[n] = Region { next: start, end };
        frames.count += 1;
        frames.free_bytes += end - start;
    }
    let free = frames.free_bytes;
    drop(frames);

    unsafe {
        HEAP.lock()
            .init(phys_to_virt(heap_phys), heap_size as usize)
    };
    HEAP_READY.store(true, Ordering::Relaxed);
    crate::arch::paging::init();
    ::log::info!(
        "mem: heap {} MiB, {} MiB free for frames",
        heap_size >> 20,
        free >> 20
    );
}

/// Allocate one zeroed 4 KiB frame (reusing freed frames first).
pub fn alloc_frame() -> u64 {
    let mut f = FRAMES.lock();
    if f.free_list != 0 {
        let phys = f.free_list;
        f.free_list = unsafe { (phys_to_virt(phys) as *const u64).read() };
        f.free_list_len -= 1;
        f.free_bytes -= PAGE;
        drop(f);
        unsafe { core::ptr::write_bytes(phys_to_virt(phys), 0, PAGE as usize) };
        return phys;
    }
    drop(f);
    alloc_frames(1)
}

/// Return a frame from `alloc_frame` (or a single-page `alloc_frames(1)`).
pub fn free_frame(phys: u64) {
    debug_assert!(phys % PAGE == 0 && phys != 0);
    let mut f = FRAMES.lock();
    unsafe { (phys_to_virt(phys) as *mut u64).write(f.free_list) };
    f.free_list = phys;
    f.free_list_len += 1;
    f.free_bytes += PAGE;
}

/// The kernel heap: (bytes in use, its size).
pub fn heap_usage() -> (u64, u64) {
    crate::arch::irq::without(|| {
        let h = HEAP.lock();
        (h.used() as u64, h.size() as u64)
    })
}

/// Free physical memory in bytes (bump regions + free list).
pub fn free_bytes() -> u64 {
    FRAMES.lock().free_bytes
}

/// Like `alloc_frames`, with the first frame aligned to `align` bytes (ARM
/// first-level page tables: 16 KiB). The skipped frames are lost.
pub fn alloc_frames_aligned(pages: u64, align: u64) -> u64 {
    let mut f = FRAMES.lock();
    let bytes = pages * PAGE;
    let count = f.count;
    for r in f.regions[..count].iter_mut() {
        let start = (r.next + align - 1) & !(align - 1);
        if start + bytes <= r.end {
            let used = start + bytes - r.next;
            r.next = start + bytes;
            f.free_bytes -= used;
            unsafe { core::ptr::write_bytes(phys_to_virt(start), 0, bytes as usize) };
            return start;
        }
    }
    panic!("out of physical memory ({pages} aligned pages requested)");
}

/// Allocate `pages` physically contiguous, zeroed 4 KiB frames. Returns the physical address.
pub fn alloc_frames(pages: u64) -> u64 {
    let mut f = FRAMES.lock();
    let bytes = pages * PAGE;
    let count = f.count;
    for r in f.regions[..count].iter_mut() {
        if r.end - r.next >= bytes {
            let phys = r.next;
            r.next += bytes;
            f.free_bytes -= bytes;
            unsafe { core::ptr::write_bytes(phys_to_virt(phys), 0, bytes as usize) };
            return phys;
        }
    }
    panic!("out of physical memory ({pages} pages requested)");
}
