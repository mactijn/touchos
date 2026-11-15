//! Loading static executables into a user address space. The file is
//! checked by touchos_elf (ELF32, ARM) before anything is mapped.

use touchos_elf::Arch;

use crate::mem::PAGE;
use crate::uabi::layout::{HEAP_BASE, IMAGE_BASE};
use crate::uabi::prot;
use crate::vm::AddressSpace;

const ARCH: Arch = Arch::Arm;

/// Map every loadable segment; returns the entry point.
pub fn load(space: &mut AddressSpace, d: &[u8]) -> Result<u64, &'static str> {
    let exe = touchos_elf::parse(d, ARCH, IMAGE_BASE..HEAP_BASE)?;
    for s in exe.segments() {
        let mut p = prot::READ;
        if s.write {
            p |= prot::WRITE;
        }
        if s.exec {
            p |= prot::EXEC;
        }
        let start = s.vaddr & !(PAGE - 1);
        let len = (s.end() - start).div_ceil(PAGE) * PAGE;
        space
            .map_init(start, len, p, (s.vaddr - start) as usize, s.data)
            .map_err(|_| "segments overlap or out of memory")?;
    }
    Ok(exe.entry)
}
