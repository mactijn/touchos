//! User address spaces.
//!
//! Every process has its own page table (`arch::paging::PageTable`), which
//! shares the kernel's mappings; the user part belongs to the process.
//!
//! The space remembers each region it mapped, so it can refuse overlaps and
//! free exactly the frames it owns when the process ends.

use alloc::vec::Vec;
use touchos_abi::Error;

use crate::arch::paging::{self, Attr, MemKind, PageTable};
use crate::mem::{self, PAGE};
use crate::uabi::layout::{NULL_GUARD_END, USER_END};
use crate::uabi::prot;

#[derive(Clone)]
pub enum Backing {
    /// Frames allocated for this space; freed with it.
    Owned,
    /// Frames owned by someone else (the kernel's KInfo page).
    Shared,
    /// A memory object; kept alive while mapped.
    Mem(#[allow(dead_code)] alloc::sync::Arc<crate::cap::MemObj>),
    /// Device registers.
    Device,
}

impl Backing {
    fn is_owned(&self) -> bool {
        matches!(self, Backing::Owned)
    }
}

#[derive(Clone)]
struct Region {
    start: u64,
    end: u64,
    backing: Backing,
}

pub struct AddressSpace {
    pt: PageTable,
    regions: Vec<Region>,
    /// Bytes of `Owned` memory mapped (for the per-process limit).
    owned_bytes: u64,
    /// Next free address for shared memory and device mappings.
    next_shared: u64,
}

/// No process may map more than this much memory of its own.
pub const MAX_OWNED_BYTES: u64 = 512 * 1024 * 1024;

fn attr_for(p: u64) -> Attr {
    Attr {
        user: true,
        write: p & prot::WRITE != 0,
        exec: p & prot::EXEC != 0,
        kind: MemKind::Normal,
    }
}

impl AddressSpace {
    pub fn new() -> AddressSpace {
        AddressSpace {
            pt: PageTable::new_user(),
            regions: Vec::new(),
            owned_bytes: 0,
            next_shared: crate::uabi::layout::SHARED_BASE,
        }
    }

    /// The page table's root, for `arch::paging::switch_to`.
    pub fn root(&self) -> u64 {
        self.pt.root()
    }

    /// Is `[start, start+len)` a page-aligned user range that doesn't touch
    /// anything mapped yet?
    fn check_free(&self, start: u64, len: u64) -> Result<u64, Error> {
        let end = start.checked_add(len).ok_or(Error::InvalidArgument)?;
        if len == 0 || start % PAGE != 0 || len % PAGE != 0 {
            return Err(Error::InvalidArgument);
        }
        if start < NULL_GUARD_END || end > USER_END {
            return Err(Error::BadAddress);
        }
        if self.regions.iter().any(|r| start < r.end && r.start < end) {
            return Err(Error::InvalidArgument);
        }
        Ok(end)
    }

    fn map_page(&mut self, va: u64, phys: u64, a: Attr) {
        self.pt.map(va, phys, a);
    }

    /// Map fresh zeroed memory. `prot` is `touchos_abi::prot` flags.
    pub fn map_new(&mut self, start: u64, len: u64, p: u64) -> Result<(), Error> {
        if p & prot::WRITE != 0 && p & prot::EXEC != 0 {
            return Err(Error::InvalidArgument); // W^X
        }
        let end = self.check_free(start, len)?;
        if self.owned_bytes + len > MAX_OWNED_BYTES || len > mem::free_bytes() / 2 {
            return Err(Error::NoMemory);
        }
        let flags = attr_for(p);
        let mut va = start;
        while va < end {
            let phys = mem::alloc_frame();
            self.map_page(va, phys, flags);
            va += PAGE;
        }
        self.owned_bytes += len;
        self.regions.push(Region {
            start,
            end,
            backing: Backing::Owned,
        });
        Ok(())
    }

    /// Map fresh memory and fill it from `data` (copied at offset `skip`
    /// into the first page). Used by the ELF loader.
    pub fn map_init(
        &mut self,
        start: u64,
        len: u64,
        p: u64,
        skip: usize,
        data: &[u8],
    ) -> Result<(), Error> {
        self.map_new(start, len, p)?;
        let mut copied = 0usize;
        let mut va = start + skip as u64;
        while copied < data.len() {
            let room = (PAGE - va % PAGE) as usize;
            let n = room.min(data.len() - copied);
            let phys = self.translate(va).ok_or(Error::BadAddress)?.0;
            unsafe {
                core::ptr::copy_nonoverlapping(data[copied..].as_ptr(), mem::phys_to_virt(phys), n);
            }
            paging::sync_direct(phys, n);
            copied += n;
            va += n as u64;
        }
        Ok(())
    }

    /// Map someone else's frames (never freed by this space), with a choice
    /// of caching (the KInfo page is uncached where caches alias: the kernel
    /// updates it while processes run).
    pub fn map_shared_as(&mut self, start: u64, frames: &[u64], p: u64, kind: MemKind) -> Result<(), Error> {
        let len = frames.len() as u64 * PAGE;
        let end = self.check_free(start, len)?;
        let flags = Attr { kind, ..attr_for(p) };
        for (i, &phys) in frames.iter().enumerate() {
            self.map_page(start + i as u64 * PAGE, phys, flags);
        }
        self.regions.push(Region {
            start,
            end,
            backing: Backing::Shared,
        });
        Ok(())
    }

    /// Physical address and attributes behind a user address.
    pub fn translate(&self, va: u64) -> Option<(u64, Attr)> {
        if va >= USER_END {
            return None;
        }
        self.pt.translate(va)
    }

    /// Copy `len` bytes from user memory at `va` (must be user-readable,
    /// and RAM: see `direct_mapped`).
    /// This must be the current address space.
    pub fn read(&self, va: u64, buf: &mut [u8]) -> Result<(), Error> {
        let end = va.checked_add(buf.len() as u64).ok_or(Error::BadAddress)?;
        if end > USER_END {
            return Err(Error::BadAddress);
        }
        paging::sync_user(va, buf.len());
        let mut done = 0usize;
        while done < buf.len() {
            let a = va + done as u64;
            let (phys, flags) = self.translate(a).ok_or(Error::BadAddress)?;
            let n = ((PAGE - a % PAGE) as usize).min(buf.len() - done);
            if !flags.user || !direct_mapped(phys, n, flags.kind) {
                return Err(Error::BadAddress);
            }
            paging::sync_direct(phys, n);
            unsafe {
                core::ptr::copy_nonoverlapping(
                    mem::phys_to_virt(phys),
                    buf[done..].as_mut_ptr(),
                    n,
                );
            }
            done += n;
        }
        Ok(())
    }

    /// Copy `data` into user memory at `va` (must be user-writable). This
    /// must be the current address space.
    pub fn write(&self, va: u64, data: &[u8]) -> Result<(), Error> {
        let end = va.checked_add(data.len() as u64).ok_or(Error::BadAddress)?;
        if end > USER_END {
            return Err(Error::BadAddress);
        }
        paging::sync_user(va, data.len());
        let mut done = 0usize;
        while done < data.len() {
            let a = va + done as u64;
            let (phys, flags) = self.translate(a).ok_or(Error::BadAddress)?;
            let n = ((PAGE - a % PAGE) as usize).min(data.len() - done);
            if !(flags.user && flags.write) || !direct_mapped(phys, n, flags.kind) {
                return Err(Error::BadAddress);
            }
            unsafe {
                core::ptr::copy_nonoverlapping(data[done..].as_ptr(), mem::phys_to_virt(phys), n);
            }
            paging::sync_direct(phys, n);
            done += n;
        }
        Ok(())
    }

    /// Map a memory object (or a page range of it) at a kernel-chosen
    /// address in the shared area. Returns the address.
    pub fn map_mem(
        &mut self,
        obj: alloc::sync::Arc<crate::cap::MemObj>,
        first_page: usize,
        pages: usize,
        p: u64,
    ) -> Result<u64, Error> {
        if p & prot::EXEC != 0 || pages == 0 || first_page + pages > obj.frames.len() {
            return Err(Error::InvalidArgument);
        }
        let len = pages as u64 * PAGE;
        let mut flags = attr_for(p);
        if obj.contiguous && !paging::DMA_COHERENT {
            // DMA memory: the device doesn't see the CPU's caches.
            flags.kind = MemKind::Device;
        }
        // ARM (FCSE): a cached object is mapped at its one address in the
        // global window in every process, so its lines are the same lines
        // whoever reads or writes them (see arch/arm/paging.rs).
        let global = paging::GLOBAL_SHARED && flags.kind == MemKind::Normal;
        let start = if global {
            obj.global_va().ok_or(Error::NoMemory)? + first_page as u64 * PAGE
        } else {
            self.next_shared
        };
        let end = self.check_free(start, len)?;
        // VIVT caches (ARM): two cached mappings of one page in one address
        // space are two sets of cache lines that miss each other's writes.
        if paging::CACHE_ALIASES
            && flags.kind == MemKind::Normal
            && self.regions.iter().any(|r| matches!(&r.backing, Backing::Mem(o) if alloc::sync::Arc::ptr_eq(o, &obj)))
        {
            return Err(Error::InvalidArgument);
        }
        for i in 0..pages {
            self.map_page(start + i as u64 * PAGE, obj.frames[first_page + i], flags);
        }
        if !global {
            self.next_shared = end + PAGE; // a gap between mappings
        }
        self.regions.push(Region {
            start,
            end,
            backing: Backing::Mem(obj),
        });
        Ok(start)
    }

    /// Map device memory at a kernel-chosen address: uncached (registers)
    /// or write-combining (a framebuffer).
    pub fn map_device(&mut self, phys: u64, len: u64, write_combining: bool) -> Result<u64, Error> {
        let first = phys & !(PAGE - 1);
        let pages = (phys + len.max(1) - first).div_ceil(PAGE);
        let start = self.next_shared;
        let end = self.check_free(start, pages * PAGE)?;
        let flags = Attr {
            kind: if write_combining { MemKind::WriteCombining } else { MemKind::Device },
            ..attr_for(prot::WRITE)
        };
        for i in 0..pages {
            self.map_page(start + i * PAGE, first + i * PAGE, flags);
        }
        self.next_shared = end + PAGE;
        self.regions.push(Region {
            start,
            end,
            backing: Backing::Device,
        });
        Ok(start + (phys - first))
    }

    /// Map one page of device registers read-only at a fixed address (the
    /// clock's counter).
    pub fn map_device_at(&mut self, va: u64, phys: u64) -> Result<(), Error> {
        let end = self.check_free(va, PAGE)?;
        let a = Attr { kind: MemKind::Device, ..attr_for(prot::READ) };
        self.map_page(va, phys & !(PAGE - 1), a);
        self.regions.push(Region { start: va, end, backing: Backing::Device });
        Ok(())
    }

    /// Remove the shared/device mapping that starts at `va`.
    pub fn unmap(&mut self, va: u64) -> Result<(), Error> {
        let i = self
            .regions
            .iter()
            .position(|r| r.start == va && matches!(r.backing, Backing::Mem(_) | Backing::Device))
            .ok_or(Error::InvalidArgument)?;
        let r = self.regions.remove(i);
        let mut a = r.start;
        while a < r.end {
            self.pt.unmap(a);
            a += PAGE;
        }
        Ok(())
    }

    pub fn owned_bytes(&self) -> u64 {
        self.owned_bytes
    }
}

impl Drop for AddressSpace {
    /// Free owned frames; the page table frees itself after this. The space
    /// must not be the active one.
    fn drop(&mut self) {
        self.pt.before_teardown();
        for r in &self.regions {
            if !r.backing.is_owned() {
                continue;
            }
            let mut va = r.start;
            while va < r.end {
                if let Some((phys, _)) = self.translate(va) {
                    mem::free_frame(phys & !(PAGE - 1));
                }
                va += PAGE;
            }
        }
    }
}

/// The kernel copies to and from user pages through the direct map, which
/// covers RAM only (on ARM exactly the board's RAM). Device pages (the
/// counter page every process has, a driver's registers, a framebuffer
/// outside RAM) are refused: a user pointer into one would make the kernel
/// fault (a panic on ARM). Uncached RAM (KInfo, DMA memory) is fine.
fn direct_mapped(phys: u64, n: usize, kind: MemKind) -> bool {
    kind == MemKind::Normal || mem::overlaps_ram(phys, n as u64)
}
