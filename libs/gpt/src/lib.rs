//! Reading a GUID partition table (never writing one): find a partition by
//! its type (touchos's own; the EFI system partition, to leave it alone)
//! and check that the
//! table is intact (both headers and their
//! CRCs, and that the backup sits at the very last block, which proves we
//! are looking at the whole disk).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;
use touchos_blockdev::{BLOCK, BlockDev};

/// EFI system partition type: C12A7328-F81F-11D2-BA4B-00A0C93EC93B.
pub const ESP_TYPE: [u8; 16] = guid_bytes(
    0xC12A7328,
    0xF81F,
    0x11D2,
    [0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B],
);

/// touchos's own partition (two copies of the system and touchos.toml):
/// 8BD27210-2197-490F-AF98-751660382348, chosen for touchos.
pub const TOUCHOS_TYPE: [u8; 16] = guid_bytes(
    0x8BD27210,
    0x2197,
    0x490F,
    [0xAF, 0x98, 0x75, 0x16, 0x60, 0x38, 0x23, 0x48],
);

/// Linux swap: 0657FD6D-A4AB-43C4-84E5-0933C84B4F4F.
pub const SWAP_TYPE: [u8; 16] = guid_bytes(
    0x0657FD6D,
    0xA4AB,
    0x43C4,
    [0x84, 0xE5, 0x09, 0x33, 0xC8, 0x4B, 0x4F, 0x4F],
);

/// A GUID in its on-disk (mixed-endian) byte order.
pub const fn guid_bytes(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [
        a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6],
        d[7],
    ]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Io,
    /// No "EFI PART" signature, bad CRC, or inconsistent headers.
    Invalid(&'static str),
    /// No partition of the requested type.
    Missing,
    /// More than one partition of the requested type.
    Several,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Partition {
    pub type_guid: [u8; 16],
    pub unique_guid: [u8; 16],
    pub first_lba: u64,
    /// Inclusive.
    pub last_lba: u64,
}

impl Partition {
    pub fn blocks(&self) -> u64 {
        self.last_lba - self.first_lba + 1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub disk_guid: [u8; 16],
    pub partitions: Vec<Partition>,
}

/// CRC-32 (IEEE, as used by GPT).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

struct Header {
    current: u64,
    backup: u64,
    first_usable: u64,
    last_usable: u64,
    disk_guid: [u8; 16],
    entries_lba: u64,
    entries: u32,
    entry_size: u32,
    entries_crc: u32,
}

fn header(dev: &mut impl BlockDev, lba: u64) -> Result<Header, Error> {
    let mut b = [0u8; BLOCK];
    dev.read(lba, &mut b).map_err(|_| Error::Io)?;
    if &b[0..8] != b"EFI PART" {
        return Err(Error::Invalid("no GPT signature"));
    }
    let size = u32_at(&b, 12) as usize;
    if !(92..=BLOCK).contains(&size) {
        return Err(Error::Invalid("bad GPT header size"));
    }
    let mut h = b[..size].to_vec();
    let crc = u32_at(&h, 16);
    h[16..20].fill(0);
    if crc32(&h) != crc {
        return Err(Error::Invalid("GPT header CRC mismatch"));
    }
    let entry_size = u32_at(&b, 84);
    let entries = u32_at(&b, 80);
    if entry_size < 128 || entry_size % 8 != 0 || entries == 0 || entries > 1024 {
        return Err(Error::Invalid("bad GPT entry array"));
    }
    Ok(Header {
        current: u64_at(&b, 24),
        backup: u64_at(&b, 32),
        first_usable: u64_at(&b, 40),
        last_usable: u64_at(&b, 48),
        disk_guid: b[56..72].try_into().unwrap(),
        entries_lba: u64_at(&b, 72),
        entries,
        entry_size,
        entries_crc: u32_at(&b, 88),
    })
}

fn entries(dev: &mut impl BlockDev, h: &Header) -> Result<Vec<u8>, Error> {
    let bytes = h.entries as usize * h.entry_size as usize;
    let mut buf = vec![0u8; bytes.div_ceil(BLOCK) * BLOCK];
    dev.read(h.entries_lba, &mut buf).map_err(|_| Error::Io)?;
    buf.truncate(bytes);
    if crc32(&buf) != h.entries_crc {
        return Err(Error::Invalid("GPT entry array CRC mismatch"));
    }
    Ok(buf)
}

/// Read and check the partition table of the whole disk `dev`.
pub fn read(dev: &mut impl BlockDev) -> Result<Table, Error> {
    let last = dev
        .blocks()
        .checked_sub(1)
        .ok_or(Error::Invalid("empty disk"))?;
    let mut mbr = [0u8; BLOCK];
    dev.read(0, &mut mbr).map_err(|_| Error::Io)?;
    if mbr[510..512] != [0x55, 0xaa] || !mbr[446..510].chunks(16).any(|e| e[4] == 0xee) {
        return Err(Error::Invalid("no protective MBR"));
    }
    let p = header(dev, 1)?;
    if p.current != 1 || p.backup != last {
        return Err(Error::Invalid("backup GPT header is not at the last block"));
    }
    let b = header(dev, last)?;
    if b.current != last
        || b.backup != 1
        || b.disk_guid != p.disk_guid
        || b.entries_crc != p.entries_crc
        || b.first_usable != p.first_usable
        || b.last_usable != p.last_usable
    {
        return Err(Error::Invalid("primary and backup GPT headers differ"));
    }
    let raw = entries(dev, &p)?;
    entries(dev, &b)?;
    let partitions = raw
        .chunks(p.entry_size as usize)
        .filter(|e| e[0..16].iter().any(|&x| x != 0))
        .map(|e| Partition {
            type_guid: e[0..16].try_into().unwrap(),
            unique_guid: e[16..32].try_into().unwrap(),
            first_lba: u64_at(e, 32),
            last_lba: u64_at(e, 40),
        })
        .collect::<Vec<_>>();
    for q in &partitions {
        if q.first_lba < p.first_usable || q.last_lba > p.last_usable || q.last_lba < q.first_lba {
            return Err(Error::Invalid("partition outside the usable area"));
        }
    }
    Ok(Table {
        disk_guid: p.disk_guid,
        partitions,
    })
}

impl Table {
    /// The one partition of type `type_guid`.
    pub fn only(&self, type_guid: [u8; 16]) -> Result<Partition, Error> {
        let mut found = self.partitions.iter().filter(|p| p.type_guid == type_guid);
        let p = *found.next().ok_or(Error::Missing)?;
        if found.next().is_some() {
            return Err(Error::Several);
        }
        Ok(p)
    }

    /// The one EFI system partition.
    pub fn esp(&self) -> Result<Partition, Error> {
        self.only(ESP_TYPE)
    }

    /// touchos's own partition.
    pub fn touchos(&self) -> Result<Partition, Error> {
        self.only(TOUCHOS_TYPE)
    }

    /// Does `p` overlap any other partition in the table? (A sane table
    /// never does; the storage server refuses to write if it did.)
    pub fn overlaps_another(&self, p: &Partition) -> bool {
        self.partitions
            .iter()
            .filter(|q| *q != p)
            .any(|q| q.first_lba <= p.last_lba && p.first_lba <= q.last_lba)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use touchos_blockdev::MemDisk;

    /// A disk image with three partitions: ESP, root, touchos.
    pub fn disk(blocks: u64) -> MemDisk {
        let mut d = MemDisk(vec![0u8; blocks as usize * BLOCK]);
        let parts = [
            (ESP_TYPE, 2048u64, 4095u64),
            (
                guid_bytes(
                    0x0FC63DAF,
                    0x8483,
                    0x4772,
                    [0x8E, 0x79, 0x3D, 0x69, 0xD8, 0x47, 0x7D, 0xE4],
                ),
                4096,
                8191,
            ),
            (TOUCHOS_TYPE, 8192, blocks - 40),
        ];
        let mut ent = vec![0u8; 128 * 128];
        for (i, (t, f, l)) in parts.iter().enumerate() {
            let e = &mut ent[i * 128..];
            e[0..16].copy_from_slice(t);
            e[16] = i as u8 + 1;
            e[32..40].copy_from_slice(&f.to_le_bytes());
            e[40..48].copy_from_slice(&l.to_le_bytes());
        }
        let ecrc = crc32(&ent);
        let last = blocks - 1;
        let hdr = |cur: u64, bak: u64, el: u64| {
            let mut h = vec![0u8; BLOCK];
            h[0..8].copy_from_slice(b"EFI PART");
            h[8..12].copy_from_slice(&0x10000u32.to_le_bytes());
            h[12..16].copy_from_slice(&92u32.to_le_bytes());
            h[24..32].copy_from_slice(&cur.to_le_bytes());
            h[32..40].copy_from_slice(&bak.to_le_bytes());
            h[40..48].copy_from_slice(&34u64.to_le_bytes());
            h[48..56].copy_from_slice(&(last - 33).to_le_bytes());
            h[56..72].copy_from_slice(&[0xAB; 16]);
            h[72..80].copy_from_slice(&el.to_le_bytes());
            h[80..84].copy_from_slice(&128u32.to_le_bytes());
            h[84..88].copy_from_slice(&128u32.to_le_bytes());
            h[88..92].copy_from_slice(&ecrc.to_le_bytes());
            let c = crc32(&h[..92]);
            h[16..20].copy_from_slice(&c.to_le_bytes());
            h
        };
        d.0[446 + 4] = 0xee;
        d.0[510] = 0x55;
        d.0[511] = 0xaa;
        d.write(1, &hdr(1, last, 2)).unwrap();
        d.write(2, &ent).unwrap();
        d.write(last - 32, &ent).unwrap();
        d.write(last, &hdr(last, 1, last - 32)).unwrap();
        d
    }

    #[test]
    fn crc32_known_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn finds_the_esp() {
        let mut d = disk(20_000);
        let t = read(&mut d).unwrap();
        assert_eq!(t.partitions.len(), 3);
        let esp = t.esp().unwrap();
        assert_eq!(
            (esp.first_lba, esp.last_lba, esp.blocks()),
            (2048, 4095, 2048)
        );
        assert_eq!(t.disk_guid, [0xAB; 16]);
        let tp = t.touchos().unwrap();
        assert_eq!((tp.first_lba, tp.last_lba), (8192, 20_000 - 40));
        assert!(!t.overlaps_another(&tp));
        assert_eq!(t.only(SWAP_TYPE), Err(Error::Missing));
        let mut two = t.clone();
        two.partitions.push(tp);
        assert_eq!(two.touchos(), Err(Error::Several));
        let mut wide = t.clone();
        wide.partitions[2].first_lba = 4000; // reaches into the root partition
        let wp = wide.partitions[2];
        assert!(wide.overlaps_another(&wp));
    }

    #[test]
    fn rejects_damage() {
        let mut d = disk(20_000);
        d.0[2 * BLOCK + 40] ^= 1; // an entry changed: CRC mismatch
        assert!(matches!(read(&mut d), Err(Error::Invalid(_))));
        // A disk that is bigger than the table says (e.g. we read a
        // different device): the backup isn't at the end.
        let mut d = disk(20_000);
        d.0.extend_from_slice(&[0u8; 8 * BLOCK]);
        assert!(matches!(read(&mut d), Err(Error::Invalid(_))));
        let mut d = disk(20_000);
        d.0[1 * BLOCK] = b'X';
        assert!(matches!(read(&mut d), Err(Error::Invalid(_))));
    }
}
