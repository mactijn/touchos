use super::*;

const AREA: Range<u64> = 0x40_0000..0x1000_0000;

/// A program header: (type, flags, offset, vaddr, filesz, memsz).
type Ph = (u32, u32, u64, u64, u64, u64);

/// An executable for `arch`: the header, the program headers right after
/// it, then `body` (at offset 0x200).
fn exe(arch: Arch, entry: u64, phs: &[Ph], body: &[u8]) -> Vec<u8> {
    let mut d = vec![0u8; 0x200];
    d[0..4].copy_from_slice(b"\x7fELF");
    d[4] = arch.class();
    d[5] = 1;
    d[16..18].copy_from_slice(&2u16.to_le_bytes());
    d[18..20].copy_from_slice(&arch.machine().to_le_bytes());
    let phoff = if arch.wide() { 64 } else { 52 };
    let n = phs.len() as u16;
    if arch.wide() {
        d[24..32].copy_from_slice(&entry.to_le_bytes());
        d[32..40].copy_from_slice(&(phoff as u64).to_le_bytes());
        d[54..56].copy_from_slice(&56u16.to_le_bytes());
        d[56..58].copy_from_slice(&n.to_le_bytes());
    } else {
        d[24..28].copy_from_slice(&(entry as u32).to_le_bytes());
        d[28..32].copy_from_slice(&(phoff as u32).to_le_bytes());
        d[42..44].copy_from_slice(&32u16.to_le_bytes());
        d[44..46].copy_from_slice(&n.to_le_bytes());
    }
    for (i, &(ty, flags, off, vaddr, filesz, memsz)) in phs.iter().enumerate() {
        let at = phoff + i * arch.phsize();
        let mut put = |o: usize, v: u64, wide: bool| {
            let b = v.to_le_bytes();
            let n = if wide { 8 } else { 4 };
            d[at + o..at + o + n].copy_from_slice(&b[..n]);
        };
        put(0, ty as u64, false);
        if arch.wide() {
            put(4, flags as u64, false);
            for (o, v) in [(8, off), (16, vaddr), (24, vaddr), (32, filesz), (40, memsz)] {
                put(o, v, true);
            }
        } else {
            for (o, v) in [(4, off), (8, vaddr), (12, vaddr), (16, filesz), (20, memsz)] {
                put(o, v, false);
            }
            put(24, flags as u64, false);
        }
    }
    d.extend_from_slice(body);
    d
}

const R: u32 = 4;
const RX: u32 = R | PF_X;
const RW: u32 = R | PF_W;

/// A program as the linker scripts make them: text, data with bss, and a
/// note (not loaded).
fn program(arch: Arch) -> Vec<u8> {
    exe(
        arch,
        0x40_0010,
        &[
            (PT_LOAD, RX, 0x200, 0x40_0000, 0x40, 0x40),
            (4, R, 0x200, 0, 4, 4),
            (PT_LOAD, RW, 0x240, 0x40_1000, 0x10, 0x1000),
            (PT_LOAD, R, 0x240, 0x40_3000, 0, 0),
        ],
        &[0xaa; 0x50],
    )
}

#[test]
fn programs_of_both_kinds() {
    for arch in [Arch::X86_64, Arch::Arm] {
        let d = program(arch);
        let e = parse(&d, arch, AREA).unwrap();
        assert_eq!(e.entry, 0x40_0010);
        let s: Vec<Segment> = e.segments().collect();
        assert_eq!(s.len(), 2); // the note and the empty segment are skipped
        assert_eq!((s[0].vaddr, s[0].memsz, s[0].data.len(), s[0].write, s[0].exec), (0x40_0000, 0x40, 0x40, false, true));
        assert_eq!((s[1].vaddr, s[1].end(), s[1].data.len(), s[1].write, s[1].exec), (0x40_1000, 0x40_2000, 0x10, true, false));
        // The other architecture's kernel refuses it.
        let other = if arch == Arch::Arm { Arch::X86_64 } else { Arch::Arm };
        assert!(parse(&d, other, AREA).is_err());
    }
}

#[test]
fn cut_anywhere() {
    for arch in [Arch::X86_64, Arch::Arm] {
        let d = program(arch);
        for n in 0..d.len() {
            assert!(parse(&d[..n], arch, AREA).is_err(), "{arch:?} cut at {n}");
        }
    }
}

#[test]
fn headers_that_lie() {
    let arch = Arch::X86_64;
    let good = program(arch);
    let with = |f: &dyn Fn(&mut Vec<u8>)| {
        let mut d = good.clone();
        f(&mut d);
        parse(&d, arch, AREA).err()
    };
    assert_eq!(with(&|d| d[0] = 0), Some("not a little-endian ELF file of this word size"));
    assert_eq!(with(&|d| d[5] = 2), Some("not a little-endian ELF file of this word size")); // big-endian
    assert_eq!(with(&|d| d[16] = 3), Some("not an x86-64 executable (ET_EXEC)")); // shared object
    // Program headers far beyond the file, or so far the sum overflows.
    assert_eq!(with(&|d| d[32..40].copy_from_slice(&0x10_0000u64.to_le_bytes())), Some(BAD));
    assert_eq!(with(&|d| d[32..40].copy_from_slice(&u64::MAX.to_le_bytes())), Some(BAD));
    assert_eq!(with(&|d| d[32..40].copy_from_slice(&(u64::MAX - 8).to_le_bytes())), Some(BAD));
    // A header size too small for a program header; too many of them.
    assert_eq!(with(&|d| d[54] = 32), Some(BAD));
    assert_eq!(with(&|d| d[56] = 33), Some(BAD));
}

#[test]
fn segments_that_lie() {
    let arch = Arch::X86_64;
    let one = |ph: Ph| parse(&exe(arch, 0x40_0000, &[ph], &[0; 0x40]), arch, AREA).err();
    assert_eq!(one((PT_LOAD, RX, 0x200, 0x40_0000, 0x40, 0x40)), None);
    // Data past the end of the file; an offset that overflows.
    assert_eq!(one((PT_LOAD, RX, 0x200, 0x40_0000, 0x41, 0x100)), Some(BAD));
    assert_eq!(one((PT_LOAD, RX, u64::MAX, 0x40_0000, 2, 0x100)), Some(BAD));
    // More in the file than in memory.
    assert_eq!(one((PT_LOAD, RX, 0x200, 0x40_0000, 0x40, 0x20)), Some("segment outside the program area"));
    // Outside the program area, or wrapping around.
    assert_eq!(one((PT_LOAD, RX, 0x200, 0x3f_f000, 0x40, 0x40)), Some("segment outside the program area"));
    assert_eq!(one((PT_LOAD, RX, 0x200, 0x40_0000, 0x40, 0x1000_0000)), Some("segment outside the program area"));
    assert_eq!(one((PT_LOAD, RX, 0x200, 0x40_0000, 0x40, u64::MAX)), Some(BAD));
    // Writable and executable.
    assert_eq!(one((PT_LOAD, RX | PF_W, 0x200, 0x40_0000, 0x40, 0x40)), Some("segment is both writable and executable"));
}

#[test]
fn the_entry_point_must_be_code() {
    let arch = Arch::Arm;
    let at = |entry| {
        let d = exe(arch, entry, &[(PT_LOAD, RX, 0x200, 0x40_0000, 0x40, 0x40), (PT_LOAD, RW, 0x240, 0x40_1000, 0x10, 0x10)], &[0; 0x50]);
        parse(&d, arch, AREA).map(|e| e.entry)
    };
    assert_eq!(at(0x40_003f), Ok(0x40_003f));
    assert_eq!(at(0x40_0040), Err("entry point is not in an executable segment"));
    assert_eq!(at(0x40_1000), Err("entry point is not in an executable segment")); // data
    assert_eq!(at(0), Err("entry point is not in an executable segment"));
}
