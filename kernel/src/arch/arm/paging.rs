//! ARMv5 paging (ARM926EJ-S): one 16 KiB first-level table per address
//! space, 1 KiB coarse second-level tables, 4 KiB small pages.
//!
//! Kernel half (0xc0000000 and up):
//! - RAM at 0xc0000000 (1 MiB sections, cached): the direct map,
//!   `mem::phys_to_virt`. Set up by the start-up code (boot.rs).
//! - Kernel stacks at `KSTACK_WINDOW`, MMIO at `MMIO_WINDOW`: 4 KiB pages
//!   in second-level tables that `init` allocates for the whole windows,
//!   so the kernel's first-level entries never change after `init` and
//!   every address space (which copies them) sees later kernel mappings.
//!
//! The ARM926's MMU reads page tables from memory, not through the D-cache:
//! every descriptor written is cleaned out of the cache. Its caches are
//! virtually indexed (VIVT): a frame zeroed through the direct map has dirty
//! lines there, which are written back before the frame is mapped elsewhere.
//!
//! **FCSE** (the Fast Context Switch Extension): every user address space
//! has a process id (1-31, `PIDS`), and the CPU adds `pid × 32 MiB` to every
//! address below 32 MiB before the caches and the MMU see it (the "modified
//! virtual address", MVA). Each process's private memory (all of it lies
//! below 32 MiB: `touchos_abi::layout`) so sits at its own MVAs, its page
//! table maps those, and the caches never mix two processes' lines. A
//! switch is then: the pid, the table, an empty TLB; no cache flush (which
//! cost the whole D-cache's contents on every switch before).
//!
//! What the flush used to take care of, done where it is needed now:
//! - shared memory objects get one address in every process, in the
//!   global window (`GLOBAL_WINDOW`, above 32 MiB, so not relocated): one
//!   set of cache lines, whoever writes;
//! - the kernel touching a process's memory through the direct map cleans
//!   that process's lines by their MVA first (`sync_user`, `sync_space`);
//! - an address space's teardown, and the reuse of a pid, write back and
//!   empty the whole cache (a dead process's dirty lines must not land on
//!   frames that have a new owner).
//!
//! The cache operations by MVA need no translation (the ARM926 keeps each
//! line's physical address for its write-back), so they work for any
//! address space, active or not.

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;

use touchos_abi::imx_boot::{KERNEL_VIRT, RAM_SIZE};

use super::cache;
use crate::mem::{self, PAGE};

/// Devices don't see the caches: DMA memory is mapped uncached.
pub const DMA_COHERENT: bool = false;
/// The caches are VIVT: two virtual mappings of one frame miss each other's
/// writes, so memory both sides write while the other runs is uncached.
pub const CACHE_ALIASES: bool = true;

/// Kernel stacks live here (16 MiB).
pub const KSTACK_WINDOW: u64 = 0xd000_0000;
const KSTACK_SIZE: u64 = 16 << 20;
/// Device registers are mapped here (16 MiB).
const MMIO_WINDOW: u64 = 0xe000_0000;
const MMIO_SIZE: u64 = 16 << 20;

pub use crate::mem::{Attr, MemKind};

// Descriptor bits.
const L1_COARSE: u32 = 0b01 | 1 << 4; // coarse table, domain 0 (bit 4 SBO)
const L2_SMALL: u32 = 0b10;
const B: u32 = 1 << 2;
const C: u32 = 1 << 3;
/// AP for all four subpages: 01 = kernel read/write, user none;
/// 10 = user read-only; 11 = user read/write.
const AP_KERNEL: u32 = 0b01_01_01_01 << 4;
const AP_USER_RO: u32 = 0b10_10_10_10 << 4;
const AP_USER_RW: u32 = 0b11_11_11_11 << 4;
const AP_MASK: u32 = 0xff << 4;

fn l2_desc(phys: u64, a: Attr) -> u32 {
    let ap = match (a.user, a.write) {
        (false, _) => AP_KERNEL,
        (true, false) => AP_USER_RO,
        (true, true) => AP_USER_RW,
    };
    let cache = match a.kind {
        MemKind::Normal => C | B,
        MemKind::Device => 0,
        MemKind::WriteCombining => B,
    };
    (phys as u32 & !0xfff) | ap | cache | L2_SMALL
}

fn attr_of(desc: u32) -> Attr {
    let ap = desc & AP_MASK;
    Attr {
        user: ap == AP_USER_RO || ap == AP_USER_RW,
        write: ap != AP_USER_RO,
        exec: true,
        kind: match desc & (C | B) {
            x if x == C | B => MemKind::Normal,
            B => MemKind::WriteCombining,
            _ => MemKind::Device,
        },
    }
}

/// The kernel's first-level table: the boot table (physical address).
static KERNEL_L1: AtomicU64 = AtomicU64::new(0);
/// The active root: first-level table | pid (`PageTable::root`).
static CURRENT: AtomicU64 = AtomicU64::new(0);

/// FCSE: the address space each pid relocates to is 32 MiB.
const FCSE_SLOT: u64 = 32 << 20;
/// Pids for user spaces: 1..PIDS. Their MVAs end at 1 GiB, below the
/// per-process window (`SHARED_BASE`) and the global one.
const PIDS: u32 = 32;
/// (pids in use, pids whose lines may still be in the caches), as bits.
static PID_BITS: Mutex<(u32, u32)> = Mutex::new((1, 0));

/// Shared memory objects (cached) are mapped here, at the same address in
/// every process (vm's `map_mem`).
pub const GLOBAL_SHARED: bool = true;
const GLOBAL_WINDOW: (u64, u64) = (0x6000_0000, 0x8000_0000);
/// Free ranges of the global window, [start, end).
static GLOBAL_FREE: Mutex<alloc::vec::Vec<(u64, u64)>> = Mutex::new(alloc::vec::Vec::new());

/// The MVA the caches and the MMU see for `va` in the space of `pid`.
fn mva(pid: u32, va: u64) -> u64 {
    if va < FCSE_SLOT { va | (pid as u64) << 25 } else { va }
}

fn pid_of(root: u64) -> u32 {
    (root & 0x3fff) as u32
}

/// A pid for a new user space: one whose lines are surely out of the
/// caches if there is one, else any free one after a full flush. 0 (no
/// relocation, so a flush on every switch to or from it) when all are in
/// use.
fn alloc_pid() -> u32 {
    let mut g = PID_BITS.lock();
    let (used, dirty) = *g;
    let free = !used & ((1u64 << PIDS) - 1) as u32 & !1;
    if free == 0 {
        ::log::warn!("paging: all {} FCSE pids in use; this space flushes the caches on every switch", PIDS - 1);
        return 0;
    }
    let clean = free & !dirty;
    let pid = if clean != 0 {
        clean.trailing_zeros()
    } else {
        cache::flush_all();
        g.1 = 0;
        free.trailing_zeros()
    };
    g.0 |= 1 << pid;
    pid
}

fn free_pid(pid: u32) {
    if pid != 0 {
        let mut g = PID_BITS.lock();
        g.0 &= !(1 << pid);
        g.1 |= 1 << pid;
    }
}

/// `pages` pages of the global window for a shared memory object (and a
/// guard page after them). None when it is full.
pub fn global_alloc(pages: u64) -> Option<u64> {
    let len = (pages + 1) * PAGE;
    let mut free = GLOBAL_FREE.lock();
    if free.is_empty() && !GLOBAL_INIT.swap(true, Ordering::Relaxed) {
        free.push(GLOBAL_WINDOW);
    }
    let i = free.iter().position(|&(s, e)| e - s >= len)?;
    let start = free[i].0;
    free[i].0 += len;
    if free[i].0 == free[i].1 {
        free.remove(i);
    }
    // Lines a former user of the range left behind (unmapping cleaned
    // them; this is the second line of defence).
    cache::clean_invalidate_dcache(start as usize, (pages * PAGE) as usize);
    Some(start)
}
static GLOBAL_INIT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Give back a range from `global_alloc`.
pub fn global_free(start: u64, pages: u64) {
    let len = (pages + 1) * PAGE;
    let mut free = GLOBAL_FREE.lock();
    free.push((start, start + len));
    free.sort_unstable();
    // Merge neighbours.
    let mut out: alloc::vec::Vec<(u64, u64)> = alloc::vec::Vec::with_capacity(free.len());
    for &(s, e) in free.iter() {
        match out.last_mut() {
            Some(last) if last.1 == s => last.1 = e,
            _ => out.push((s, e)),
        }
    }
    *free = out;
}
static PAGING: Mutex<()> = Mutex::new(());
/// MMIO pages mapped so far, (physical page, virtual page), and the next
/// free virtual page.
static MMIO: Mutex<(alloc::vec::Vec<(u64, u64)>, u64)> =
    Mutex::new((alloc::vec::Vec::new(), MMIO_WINDOW));

/// Before `mem::init`: the boot page table the start-up code made.
pub fn set_boot_table(phys: u64) {
    KERNEL_L1.store(phys, Ordering::Relaxed);
}

pub fn kernel_root() -> u64 {
    KERNEL_L1.load(Ordering::Relaxed)
}

fn l1(root: u64) -> *mut u32 {
    mem::phys_to_virt(root) as *mut u32
}

/// After the heap is up (from `mem::init`): drop the start-up code's
/// identity mapping of RAM and give the kernel windows their second-level
/// tables.
pub fn init() {
    let root = l1(kernel_root());
    let ram_base = mem::virt_to_phys(KERNEL_VIRT as usize as *const u8);
    for i in 0..RAM_SIZE >> 20 {
        let idx = ((ram_base >> 20) + i) as usize;
        if idx < 0xc00 {
            unsafe { root.add(idx).write_volatile(0) };
        }
    }
    let mut tables = 0;
    for (start, size) in [(KSTACK_WINDOW, KSTACK_SIZE), (MMIO_WINDOW, MMIO_SIZE)] {
        // One frame holds four coarse tables: four MiB.
        for mib in (0..size >> 20).step_by(4) {
            let frame = mem::alloc_frame();
            cache::clean_dcache(mem::phys_to_virt(frame) as usize, PAGE as usize);
            for k in 0..4 {
                let idx = ((start >> 20) + mib + k) as usize;
                let desc = (frame as u32 + k as u32 * 1024) | L1_COARSE;
                unsafe { root.add(idx).write_volatile(desc) };
            }
            tables += 4;
        }
    }
    cache::clean_dcache(root as usize, 16 * 1024);
    cache::invalidate_tlb();
    ::log::debug!("paging: kernel table at {:#x}, {tables} window tables", kernel_root());
}

/// The second-level entry for `va` in the table at `root`, if `va`'s MiB
/// has a coarse table.
fn l2_entry(root: u64, va: u64) -> Option<*mut u32> {
    let d = unsafe { l1(root).add((va >> 20) as usize).read_volatile() };
    if d & 0b11 != 0b01 {
        return None;
    }
    let table = mem::phys_to_virt((d & !0x3ff) as u64) as *mut u32;
    Some(unsafe { table.add(((va >> 12) & 0xff) as usize) })
}

/// Write a second-level entry: through to memory, and out of the TLB.
fn set_entry(e: *mut u32, desc: u32, va: u64) {
    unsafe { e.write_volatile(desc) };
    cache::clean_dcache(e as usize, 4);
    cache::invalidate_tlb_entry(va as usize);
}

/// Make `phys`'s page safe to use at another address: write back and drop
/// its lines under the direct map (it was just zeroed there).
fn release_direct_map(phys: u64) {
    cache::clean_invalidate_dcache(mem::phys_to_virt(phys) as usize, PAGE as usize);
}

/// Map `frames` at kernel virtual address `va` (kernel stacks).
pub fn map_kernel(va: u64, frames: &[u64]) {
    let _guard = PAGING.lock();
    let a = Attr { user: false, write: true, exec: false, kind: MemKind::Normal };
    for (i, &phys) in frames.iter().enumerate() {
        let v = va + i as u64 * PAGE;
        release_direct_map(phys);
        let e = l2_entry(kernel_root(), v).expect("map_kernel: outside the kernel windows");
        set_entry(e, l2_desc(phys, a), v);
    }
}

/// Map device registers at `phys` (uncached) and return their virtual
/// address. Mapping the same range again returns the same address.
pub fn map_mmio(phys: u64, len: usize) -> usize {
    let _guard = PAGING.lock();
    let first = phys & !(PAGE - 1);
    let pages = (phys + len.max(1) as u64 - first).div_ceil(PAGE);
    let mut m = MMIO.lock();
    let (list, next) = &mut *m;
    // Already mapped as one run?
    if let Some(&(_, va)) = list.iter().find(|&&(p, _)| p == first)
        && (0..pages).all(|i| list.iter().any(|&(p, v)| p == first + i * PAGE && v == va + i * PAGE))
    {
        return (va + (phys - first)) as usize;
    }
    let va = *next;
    assert!(va + pages * PAGE <= MMIO_WINDOW + MMIO_SIZE, "MMIO window full");
    *next += pages * PAGE;
    let a = Attr { user: false, write: true, exec: false, kind: MemKind::Device };
    for i in 0..pages {
        let e = l2_entry(kernel_root(), va + i * PAGE).unwrap();
        set_entry(e, l2_desc(first + i * PAGE, a), va + i * PAGE);
        list.push((first + i * PAGE, va + i * PAGE));
    }
    (va + (phys - first)) as usize
}

/// Map the vector page (`frame`) at 0xffff0000: readable from user mode,
/// for the atomic helpers in it.
pub fn map_vectors(frame: u64) {
    let _guard = PAGING.lock();
    let root = l1(kernel_root());
    let tables = mem::alloc_frame(); // its first coarse table covers 0xfff00000
    cache::clean_dcache(mem::phys_to_virt(tables) as usize, PAGE as usize);
    unsafe { root.add(0xfff).write_volatile(tables as u32 | L1_COARSE) };
    cache::clean_dcache(unsafe { root.add(0xfff) } as usize, 4);
    let a = Attr { user: true, write: false, exec: true, kind: MemKind::Normal };
    let e = l2_entry(kernel_root(), 0xffff_0000).unwrap();
    set_entry(e, l2_desc(frame, a), 0xffff_0000);
    cache::invalidate_tlb();
}

/// The active root (first-level table | pid).
fn active_root() -> u64 {
    match CURRENT.load(Ordering::Relaxed) {
        0 => kernel_root(),
        r => r,
    }
}

/// Make `root` (`PageTable::root`, or `kernel_root`) the active address
/// space: its pid, its table, an empty TLB. With FCSE the caches keep
/// their lines: every space's private lines are at its own MVAs, and
/// shared memory has one address everywhere. Only a space without a pid
/// (pid 0, when they ran out) needs the old full flush, to and from it.
pub fn switch_to(root: u64) {
    let old = active_root();
    if old == root {
        return;
    }
    super::super::SWITCHES.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let unrelocated = |r: u64| pid_of(r) == 0 && r != kernel_root();
    if unrelocated(old) || unrelocated(root) {
        cache::flush_all();
    }
    let table = (root & !0x3fff) as u32;
    let pid = pid_of(root) << 25;
    unsafe {
        core::arch::asm!(
            "mcr p15, 0, {t}, c2, c0, 0", // the table
            "mcr p15, 0, {p}, c13, c0, 0", // the FCSE pid
            "mcr p15, 0, {z}, c8, c7, 0", // an empty TLB
            t = in(reg) table,
            p = in(reg) pid,
            z = in(reg) 0u32,
            options(nostack)
        )
    };
    CURRENT.store(root, Ordering::Relaxed);
}

/// Freed first-level tables (16 KiB, 16 KiB aligned), for reuse.
static L1_POOL: Mutex<alloc::vec::Vec<u64>> = Mutex::new(alloc::vec::Vec::new());

/// User space is the lower 2 GiB (`touchos_abi::layout::USER_END`).
const USER_L1_ENTRIES: usize = 0x800;

/// The page table of one user address space. Its entries are at the
/// MVAs of its pid (`mva`).
pub struct PageTable {
    l1: u64,
    pid: u32,
}

impl PageTable {
    /// A new, empty user page table that shares the kernel's half.
    pub fn new_user() -> PageTable {
        let l1 = L1_POOL.lock().pop().unwrap_or_else(|| mem::alloc_frames_aligned(4, 16 * 1024));
        let dst = mem::phys_to_virt(l1) as *mut u32;
        let src = mem::phys_to_virt(kernel_root()) as *const u32;
        unsafe {
            core::ptr::write_bytes(dst, 0, 4096);
            core::ptr::copy_nonoverlapping(src.add(0xc00), dst.add(0xc00), 0x400);
        }
        cache::clean_dcache(dst as usize, 16 * 1024);
        PageTable { l1, pid: alloc_pid() }
    }

    /// The first-level table's physical address with the pid in its low
    /// bits (the table is 16 KiB aligned): what `switch_to` takes.
    pub fn root(&self) -> u64 {
        self.l1 | self.pid as u64
    }

    fn is_active(&self) -> bool {
        active_root() == self.root()
    }

    /// Before the space's frames are freed: its lines may be dirty, and
    /// must not be written back over a frame's next owner.
    pub fn before_teardown(&self) {
        cache::flush_all();
    }

    /// Map one 4 KiB user page.
    pub fn map(&mut self, va: u64, phys: u64, a: Attr) {
        let va = mva(self.pid, va);
        let idx = (va >> 20) as usize;
        assert!(idx < USER_L1_ENTRIES, "vm: user address {va:#x} out of range");
        let root = l1(self.l1);
        if unsafe { root.add(idx).read_volatile() } & 0b11 != 0b01 {
            // One frame: the coarse tables of an aligned group of four MiB.
            let frame = mem::alloc_frame();
            cache::clean_dcache(mem::phys_to_virt(frame) as usize, PAGE as usize);
            let group = idx & !3;
            for k in 0..4 {
                unsafe { root.add(group + k).write_volatile((frame as u32 + k as u32 * 1024) | L1_COARSE) };
            }
            cache::clean_dcache(unsafe { root.add(group) } as usize, 16);
        }
        // Any RAM page (also one mapped uncached, for DMA): its dirty lines
        // under the direct map would otherwise be written back over it later.
        if crate::mem::overlaps_ram(phys, PAGE) {
            release_direct_map(phys);
        }
        let e = l2_entry(self.l1, va).unwrap();
        unsafe { e.write_volatile(l2_desc(phys, a)) };
        cache::clean_dcache(e as usize, 4);
        if self.is_active() {
            cache::invalidate_tlb_entry(va as usize);
        }
    }

    /// Physical address and attributes behind a user address.
    pub fn translate(&self, va: u64) -> Option<(u64, Attr)> {
        if va >= FCSE_SLOT && (va >> 20) as usize >= USER_L1_ENTRIES {
            return None;
        }
        let d = unsafe { l2_entry(self.l1, mva(self.pid, va))?.read_volatile() };
        (d & 0b11 == L2_SMALL).then(|| ((d & !0xfff) as u64 | (va & 0xfff), attr_of(d)))
    }

    /// Remove the mapping of one page. Returns the physical address that
    /// was mapped.
    pub fn unmap(&mut self, va: u64) -> Option<u64> {
        let va = mva(self.pid, va);
        if (va >> 20) as usize >= USER_L1_ENTRIES {
            return None;
        }
        let e = l2_entry(self.l1, va)?;
        let d = unsafe { e.read_volatile() };
        if d & 0b11 != L2_SMALL {
            return None;
        }
        // Its dirty lines belong to the page: write them back first (by
        // MVA, so also when the space isn't the active one).
        cache::clean_invalidate_dcache(va as usize, PAGE as usize);
        unsafe { e.write_volatile(0) };
        cache::clean_dcache(e as usize, 4);
        if self.is_active() {
            cache::invalidate_tlb_entry(va as usize);
        }
        Some((d & !0xfff) as u64)
    }
}

impl Drop for PageTable {
    /// Free the second-level tables and keep the first-level one for
    /// reuse. The table must not be the active one.
    fn drop(&mut self) {
        debug_assert!(!self.is_active());
        let root = l1(self.l1);
        for group in (0..USER_L1_ENTRIES).step_by(4) {
            let d = unsafe { root.add(group).read_volatile() };
            if d & 0b11 == 0b01 {
                mem::free_frame((d & !0xfff) as u64);
            }
        }
        L1_POOL.lock().push(self.l1);
        free_pid(self.pid);
    }
}

/// Before the kernel touches `len` bytes of the current address space's
/// memory at user address `va` through the direct map: write back and drop
/// the lines under `va` (VIVT caches alias), by their MVA.
pub fn sync_user(va: u64, len: usize) {
    sync_space(active_root(), va, len);
}

/// The same for the address space `root` (`PageTable::root`), active or
/// not: before the kernel writes into another process's memory (a message
/// into its IPC buffer), that process's lines there go back and away.
pub fn sync_space(root: u64, va: u64, len: usize) {
    if (va >> 20) < USER_L1_ENTRIES as u64 && len > 0 {
        cache::clean_invalidate_dcache(mva(pid_of(root), va) as usize, len);
    }
}

/// Before reading or after writing physical memory through the direct
/// map, when the same memory is also mapped somewhere else.
pub fn sync_direct(phys: u64, len: usize) {
    if len > 0 {
        cache::clean_invalidate_dcache(mem::phys_to_virt(phys) as usize, len);
    }
}

/// Before the kernel reads memory that a process may have written at an
/// address it doesn't know (a memory object): everything out of the cache.
pub fn sync_all() {
    cache::flush_all();
}
