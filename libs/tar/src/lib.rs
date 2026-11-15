//! The initrd's archive format: ustar, regular files only.
//!
//! One reader for both of its users (the kernel finds init in it, init
//! finds the other programs and the firmware), and the writer xtask builds
//! the initrd with, so the two can't drift apart.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec::Vec;

/// Headers and contents are padded to whole blocks.
pub const BLOCK: usize = 512;

/// The longest name the writer stores (the header's name field, with its NUL).
pub const NAME_MAX: usize = 99;

// Header fields: (offset, length).
const NAME: (usize, usize) = (0, 100);
const MODE: (usize, usize) = (100, 8);
const UID: (usize, usize) = (108, 8);
const GID: (usize, usize) = (116, 8);
const SIZE: (usize, usize) = (124, 12);
const MTIME: (usize, usize) = (136, 12);
const CHECKSUM: (usize, usize) = (148, 8);
const TYPE: usize = 156;
const MAGIC: (usize, usize) = (257, 8);

/// One regular file of an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct File<'a> {
    pub name: &'a str,
    /// Where the contents start in the archive.
    pub offset: usize,
    pub data: &'a [u8],
}

/// The regular files of `tar`, in archive order. Reading stops at the end
/// marker (a zero block), at a damaged header, and at a file that would
/// run past the end of `tar`.
pub fn files(tar: &[u8]) -> Files<'_> {
    Files { tar, off: 0 }
}

/// The contents of the regular file `name`.
pub fn find<'a>(tar: &'a [u8], name: &str) -> Option<&'a [u8]> {
    files(tar).find(|f| f.name == name).map(|f| f.data)
}

pub struct Files<'a> {
    tar: &'a [u8],
    off: usize,
}

impl<'a> Iterator for Files<'a> {
    type Item = File<'a>;

    fn next(&mut self) -> Option<File<'a>> {
        loop {
            let (file, regular, next) = entry(self.tar, self.off)?;
            self.off = next;
            if regular {
                return Some(file);
            }
        }
    }
}

/// The entry at `off`: the file, whether it is a regular file, and where
/// the next header starts. None at the end or on damage.
fn entry(tar: &[u8], off: usize) -> Option<(File<'_>, bool, usize)> {
    let h = tar.get(off..off.checked_add(BLOCK)?)?;
    if h.iter().all(|&b| b == 0) {
        return None;
    }
    let name = field(h, NAME);
    let name_len = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    let name = core::str::from_utf8(&name[..name_len]).ok()?;
    let size = octal(field(h, SIZE))?;
    let start = off + BLOCK;
    let data = tar.get(start..start.checked_add(size)?)?;
    let regular = h[TYPE] == b'0' || h[TYPE] == 0;
    let next = start.checked_add(size.div_ceil(BLOCK) * BLOCK)?;
    Some((
        File {
            name,
            offset: start,
            data,
        },
        regular,
        next,
    ))
}

fn field(h: &[u8], (off, len): (usize, usize)) -> &[u8] {
    &h[off..off + len]
}

/// An octal number field, padded with NULs or spaces on either side.
fn octal(f: &[u8]) -> Option<usize> {
    let s = core::str::from_utf8(f).ok()?;
    usize::from_str_radix(s.trim_matches(|c| c == '\0' || c == ' '), 8).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Longer than `NAME_MAX` bytes.
    NameTooLong,
    /// Larger than the size field holds (8 GiB).
    TooLarge,
}

/// Append one regular file: its header, then its contents padded to a
/// whole block. Every field but the name and size is fixed (mode 755,
/// owner 0, time 0), so the same files give the same archive.
pub fn append(tar: &mut Vec<u8>, name: &str, data: &[u8]) -> Result<(), Error> {
    if name.len() > NAME_MAX {
        return Err(Error::NameTooLong);
    }
    let mut h = [0u8; BLOCK];
    h[..name.len()].copy_from_slice(name.as_bytes());
    put_octal(&mut h, MODE, 0o755)?;
    put_octal(&mut h, UID, 0)?;
    put_octal(&mut h, GID, 0)?;
    put_octal(&mut h, SIZE, data.len() as u64)?;
    put_octal(&mut h, MTIME, 0)?;
    h[TYPE] = b'0';
    h[MAGIC.0..MAGIC.0 + MAGIC.1].copy_from_slice(b"ustar\x0000");
    // The checksum: the sum of all header bytes, counting its own field
    // as spaces; six digits, NUL, space.
    h[CHECKSUM.0..CHECKSUM.0 + CHECKSUM.1].fill(b' ');
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    put_octal(&mut h, (CHECKSUM.0, 7), sum as u64)?;
    tar.extend_from_slice(&h);
    tar.extend_from_slice(data);
    tar.resize(tar.len().div_ceil(BLOCK) * BLOCK, 0);
    Ok(())
}

/// End the archive: two zero blocks.
pub fn finish(tar: &mut Vec<u8>) {
    tar.resize(tar.len() + 2 * BLOCK, 0);
}

/// Zero-padded octal digits filling the field but its last byte (a NUL).
fn put_octal(h: &mut [u8], (off, len): (usize, usize), v: u64) -> Result<(), Error> {
    let digits = len - 1;
    if v >> (3 * digits) != 0 {
        return Err(Error::TooLarge);
    }
    for (i, b) in h[off..off + digits].iter_mut().enumerate() {
        *b = b'0' + (v >> (3 * (digits - 1 - i)) & 7) as u8;
    }
    h[off + digits] = 0;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = Vec::new();
        for (n, d) in files {
            append(&mut tar, n, d).unwrap();
        }
        finish(&mut tar);
        tar
    }

    #[test]
    fn reads_back_what_it_wrote() {
        let big = vec![0xa5u8; 1500];
        let tar = archive(&[
            ("init", b"\x7fELF..."),
            ("empty", b""),
            ("rt3071.bin", &big),
        ]);
        assert_eq!(tar.len() % BLOCK, 0);
        let got: Vec<File> = files(&tar).collect();
        assert_eq!(got.len(), 3);
        assert_eq!(
            (got[0].name, got[0].offset, got[0].data),
            ("init", 512, &b"\x7fELF..."[..])
        );
        assert_eq!(
            (got[1].name, got[1].offset, got[1].data.len()),
            ("empty", 1536, 0)
        );
        // An empty file has a header and no data blocks.
        assert_eq!(
            (got[2].name, got[2].offset, got[2].data.len()),
            ("rt3071.bin", 2048, 1500)
        );
        assert!(got[2].data == big && tar[2048..2048 + 1500] == big[..]);
        assert!(find(&tar, "rt3071.bin") == Some(&big[..]));
        // Padding to the next block, then the end marker.
        assert_eq!(tar.len(), 2048 + 1536 + 2 * BLOCK);
        assert_eq!(find(&tar, "nope"), None);
    }

    /// The header's bytes, as xtask's writer always made them (and as GNU
    /// tar reads them: `tar tvf build/touchos.initrd`).
    #[test]
    fn header_layout() {
        let tar = archive(&[("hello", b"hi")]);
        let h = &tar[..BLOCK];
        assert_eq!(&h[..6], b"hello\0");
        assert_eq!(&h[100..108], b"0000755\0");
        assert_eq!(&h[108..124], b"0000000\x000000000\0");
        assert_eq!(&h[124..136], b"00000000002\0");
        assert_eq!(&h[136..148], b"00000000000\0");
        assert_eq!(h[156], b'0');
        assert_eq!(&h[257..265], b"ustar\x0000");
        let mut sum: u32 = h.iter().map(|&b| b as u32).sum();
        sum = sum - h[148..156].iter().map(|&b| b as u32).sum::<u32>() + 8 * b' ' as u32;
        let want = format!("{sum:06o}\0 ");
        assert_eq!(&h[148..156], want.as_bytes());
        assert_eq!(&tar[BLOCK..BLOCK + 2], b"hi");
        assert!(tar[BLOCK + 2..].iter().all(|&b| b == 0));
        assert_eq!(tar.len(), 4 * BLOCK);
    }

    #[test]
    fn names() {
        let long = "x".repeat(NAME_MAX);
        let tar = archive(&[(&long, b"1")]);
        assert_eq!(files(&tar).next().unwrap().name, long);
        assert_eq!(
            append(&mut Vec::new(), &"x".repeat(NAME_MAX + 1), b""),
            Err(Error::NameTooLong)
        );
        // Another writer may use all 100 bytes, without a NUL.
        let mut tar = archive(&[("a", b"1")]);
        tar[..100].fill(b'n');
        assert_eq!(files(&tar).next().unwrap().name.len(), 100);
    }

    #[test]
    fn other_writers_fields() {
        // GNU tar: size with a trailing space instead of a NUL, a
        // directory entry (type '5') before the file.
        let mut tar = archive(&[("dir/", b""), ("dir/f", b"abc")]);
        tar[156] = b'5';
        tar[BLOCK + 124..BLOCK + 136].copy_from_slice(b"00000000003 ");
        let got: Vec<&str> = files(&tar).map(|f| f.name).collect();
        assert_eq!(got, ["dir/f"]);
        assert_eq!(find(&tar, "dir/f"), Some(&b"abc"[..]));
    }

    #[test]
    fn damage_ends_the_listing() {
        let tar = archive(&[("a", b"1"), ("b", &[2; 600])]);
        // Cut into b's contents: a is still there, b isn't.
        let cut = &tar[..BLOCK * 3 + 10];
        let got: Vec<&str> = files(cut).map(|f| f.name).collect();
        assert_eq!(got, ["a"]);
        // A size that isn't octal.
        let mut bad = tar.clone();
        bad[2 * BLOCK + 124] = b'9';
        assert_eq!(files(&bad).count(), 1);
        // A size that would overflow the offset arithmetic.
        let mut huge = tar.clone();
        huge[2 * BLOCK + 124..2 * BLOCK + 136].copy_from_slice(b"77777777777\0");
        assert_eq!(files(&huge).count(), 1);
        // Not even one whole header.
        assert_eq!(files(&tar[..100]).count(), 0);
        assert_eq!(files(&[]).count(), 0);
    }
}
