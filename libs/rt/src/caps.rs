//! Named capabilities from our BootInfo, and memory mappings.

use alloc::vec::Vec;
use touchos_abi::{Error, prot, rights};

use crate::{start::boot_info, sys};

fn name_of(b: &[u8; 16]) -> &str {
    let n = b.iter().position(|&c| c == 0).unwrap_or(16);
    core::str::from_utf8(&b[..n]).unwrap_or("")
}

/// The slot of the capability the spawner gave us under `name`.
pub fn cap(name: &str) -> Option<u32> {
    let bi = boot_info()?;
    bi.caps[..bi.ncaps as usize]
        .iter()
        .find(|e| name_of(&e.name) == name)
        .map(|e| e.slot)
}

/// Every named capability: (name, slot, kind).
pub fn caps() -> impl Iterator<Item = (&'static str, u32, u32)> {
    boot_info()
        .into_iter()
        .flat_map(|bi| bi.caps[..bi.ncaps as usize].iter())
        .map(|e| (name_of(&e.name), e.slot, e.kind))
}

/// A new read-only memory object holding a copy of `data` (to hand to
/// another process); its capability slot. Nothing is left behind when it
/// fails.
pub fn share(data: &[u8]) -> Option<u32> {
    let len = (data.len() as u64).max(1);
    let mem = sys::mem_create(len, 0).ok()?;
    let ro = sys::mem_map(mem, 0, len, prot::WRITE).ok().and_then(|va| {
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), va as *mut u8, data.len()) };
        let _ = sys::mem_unmap(va);
        sys::cap_mint(mem, rights::MAP_READ, 0).ok()
    });
    let _ = sys::cap_drop(mem);
    ro
}

/// The contents of the memory object `mem` (e.g. what a reply brought),
/// copied out; `mem` is dropped either way.
pub fn read_shared(mem: u32) -> Option<Vec<u8>> {
    let data = Mapping::map(mem, false).ok().map(|m| m.bytes().to_vec());
    let _ = sys::cap_drop(mem);
    data
}

/// A memory object mapped into our address space.
pub struct Mapping {
    pub va: u64,
    pub len: usize,
}

impl Mapping {
    /// Map a whole memory object (read-only unless `writable`).
    pub fn map(mem: u32, writable: bool) -> Result<Mapping, Error> {
        let (_, _, len) = sys::cap_info(mem)?;
        let p = if writable { prot::WRITE } else { prot::READ };
        let va = sys::mem_map(mem, 0, len, p)?;
        Ok(Mapping {
            va,
            len: len as usize,
        })
    }

    pub fn bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.va as *const u8, self.len) }
    }

    pub fn ptr<T>(&self) -> *mut T {
        self.va as *mut T
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        let _ = sys::mem_unmap(self.va);
    }
}
