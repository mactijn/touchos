//! Static executables as the kernel loads them (built with libs/rt/user.ld):
//! the ELF header and the PT_LOAD segments, checked
//! before anything is mapped. touchos runs ELF32 ARM programs; the parser
//! also knows ELF64 (x86-64), whose 64-bit fields its tests use to check
//! the overflow cases.
//!
//! Any process may ask the kernel to start a program from bytes it
//! supplies, so every field is untrusted: offsets and sizes are checked
//! against the file and against overflow, segments against the program
//! area, and none may be writable and executable.

#![cfg_attr(not(test), no_std)]

use core::ops::Range;

const PT_LOAD: u32 = 1;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
/// More program headers than any of our programs has.
const MAX_PHNUM: usize = 32;

/// The ELF class and machine a kernel runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    /// ELF64, EM_X86_64.
    X86_64,
    /// ELF32, EM_ARM.
    Arm,
}

impl Arch {
    fn wide(self) -> bool {
        self == Arch::X86_64
    }
    fn class(self) -> u8 {
        if self.wide() { 2 } else { 1 }
    }
    fn machine(self) -> u16 {
        if self.wide() { 62 } else { 40 }
    }
    /// The size of a program header.
    fn phsize(self) -> usize {
        if self.wide() { 56 } else { 32 }
    }
}

/// A loadable segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment<'a> {
    pub vaddr: u64,
    /// Its size in memory: `data`, then zeros.
    pub memsz: u64,
    pub data: &'a [u8],
    pub write: bool,
    pub exec: bool,
}

impl Segment<'_> {
    pub fn end(&self) -> u64 {
        self.vaddr + self.memsz
    }
}

/// A checked executable.
#[derive(Debug, Clone, Copy)]
pub struct Exe<'a> {
    pub entry: u64,
    d: &'a [u8],
    arch: Arch,
    phoff: usize,
    phentsize: usize,
    phnum: usize,
}

const BAD: &str = "malformed ELF file";

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(o..o.checked_add(2)?)?.try_into().ok()?))
}
fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(o..o.checked_add(4)?)?.try_into().ok()?))
}
fn u64_at(d: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(o..o.checked_add(8)?)?.try_into().ok()?))
}

/// The bytes of `d` from `off`, `len` long, as an index range.
fn range(off: u64, len: u64, d: &[u8]) -> Option<Range<usize>> {
    let end = off.checked_add(len)?;
    (end <= d.len() as u64).then(|| off as usize..end as usize)
}

/// Check `d` as an executable for `arch` whose segments all lie in
/// `area`, with its entry point in an executable segment.
pub fn parse(d: &[u8], arch: Arch, area: Range<u64>) -> Result<Exe<'_>, &'static str> {
    if d.len() < 64 || d[0..4] != *b"\x7fELF" || d[4] != arch.class() || d[5] != 1 {
        return Err("not a little-endian ELF file of this word size");
    }
    if u16_at(d, 16) != Some(2) || u16_at(d, 18) != Some(arch.machine()) {
        return Err(match arch {
            Arch::X86_64 => "not an x86-64 executable (ET_EXEC)",
            Arch::Arm => "not an ARM executable (ET_EXEC)",
        });
    }
    let word = |o| if arch.wide() { u64_at(d, o) } else { u32_at(d, o).map(u64::from) };
    let entry = word(24).ok_or(BAD)?;
    let (phoff, phentsize, phnum) = if arch.wide() {
        (u64_at(d, 32), u16_at(d, 54), u16_at(d, 56))
    } else {
        (u32_at(d, 28).map(u64::from), u16_at(d, 42), u16_at(d, 44))
    };
    let (phoff, phentsize, phnum) = (phoff.ok_or(BAD)?, phentsize.ok_or(BAD)? as usize, phnum.ok_or(BAD)? as usize);
    if phentsize < arch.phsize() || phnum > MAX_PHNUM {
        return Err(BAD);
    }
    // All the program headers inside the file.
    let all = range(phoff, (phentsize * phnum) as u64, d).ok_or(BAD)?;
    let exe = Exe { entry, d, arch, phoff: all.start, phentsize, phnum };
    let mut entry_ok = false;
    for i in 0..phnum {
        let Some(s) = exe.segment(i)? else { continue };
        if s.vaddr < area.start || s.end() > area.end {
            return Err("segment outside the program area");
        }
        if s.write && s.exec {
            return Err("segment is both writable and executable");
        }
        entry_ok |= s.exec && (s.vaddr..s.end()).contains(&entry);
    }
    if !entry_ok {
        return Err("entry point is not in an executable segment");
    }
    Ok(exe)
}

impl<'a> Exe<'a> {
    /// Program header `i`: Some for a loadable segment that takes memory.
    fn segment(&self, i: usize) -> Result<Option<Segment<'a>>, &'static str> {
        let (d, wide) = (self.d, self.arch.wide());
        let ph = &d[self.phoff + i * self.phentsize..][..self.arch.phsize()];
        if u32_at(ph, 0) != Some(PT_LOAD) {
            return Ok(None);
        }
        let word = |o| if wide { u64_at(ph, o) } else { u32_at(ph, o).map(u64::from) };
        // ELF64: type, flags, offset, vaddr, paddr, filesz, memsz.
        // ELF32: type, offset, vaddr, paddr, filesz, memsz, flags.
        let (flags, offset, vaddr, filesz, memsz) = if wide {
            (u32_at(ph, 4), word(8), word(16), word(32), word(40))
        } else {
            (u32_at(ph, 24), word(4), word(8), word(16), word(20))
        };
        let (flags, offset, vaddr) = (flags.ok_or(BAD)?, offset.ok_or(BAD)?, vaddr.ok_or(BAD)?);
        let (filesz, memsz) = (filesz.ok_or(BAD)?, memsz.ok_or(BAD)?);
        if memsz == 0 {
            return Ok(None);
        }
        vaddr.checked_add(memsz).ok_or(BAD)?;
        if filesz > memsz {
            return Err("segment outside the program area");
        }
        let data = &d[range(offset, filesz, d).ok_or(BAD)?];
        Ok(Some(Segment { vaddr, memsz, data, write: flags & PF_W != 0, exec: flags & PF_X != 0 }))
    }

    /// The loadable segments, in the file's order.
    pub fn segments(&self) -> impl Iterator<Item = Segment<'a>> + '_ {
        // Checked by parse(): no errors left.
        (0..self.phnum).filter_map(|i| self.segment(i).ok().flatten())
    }
}

#[cfg(test)]
mod tests;
