//! USB mass storage: the bulk-only transport (BOT) and the SCSI commands
//! a disk is read with. Wire formats only, tested on the host; the driver
//! is servers/usb/src/msc.rs.
//!
//! **Read-only by design:** there is no builder for a command that writes
//! (WRITE, FORMAT UNIT, SYNCHRONIZE CACHE, ...), so a driver built on this
//! can't change what is on a disk.
//!
//! A BOT command is three bulk transfers: the 31-byte Command Block
//! Wrapper (CBW) out, the data (in, here), and the 13-byte Command Status
//! Wrapper (CSW) in, whose tag must match the CBW's.

use alloc::string::String;

pub const CBW_LEN: usize = 31;
pub const CSW_LEN: usize = 13;
const CBW_SIG: u32 = 0x4342_5355; // "USBC"
const CSW_SIG: u32 = 0x5342_5355; // "USBS"

/// Interface class, subclass and protocol of a BOT disk.
pub const CLASS: u8 = 0x08;
pub const SUBCLASS_SCSI: u8 = 0x06;
pub const PROTOCOL_BOT: u8 = 0x50;

/// Class requests on endpoint 0 (to the interface).
pub const REQ_GET_MAX_LUN: u8 = 0xfe;
pub const REQ_RESET: u8 = 0xff;

/// The CBW for a command `cdb` that moves `data_len` bytes (in from the
/// device when `data_in`).
pub fn cbw(tag: u32, data_len: u32, data_in: bool, lun: u8, cdb: &[u8]) -> [u8; CBW_LEN] {
    let mut b = [0u8; CBW_LEN];
    b[0..4].copy_from_slice(&CBW_SIG.to_le_bytes());
    b[4..8].copy_from_slice(&tag.to_le_bytes());
    b[8..12].copy_from_slice(&data_len.to_le_bytes());
    b[12] = if data_in { 0x80 } else { 0 };
    b[13] = lun & 0xf;
    let n = cdb.len().min(16);
    b[14] = n as u8;
    b[15..15 + n].copy_from_slice(&cdb[..n]);
    b
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Passed,
    /// The command failed; REQUEST SENSE says why.
    Failed,
    /// The device lost track: reset recovery.
    PhaseError,
}

/// A CSW: its status and the residue (bytes not transferred). None if it
/// isn't a valid CSW for `tag` (the device is out of step: reset recovery).
pub fn parse_csw(b: &[u8], tag: u32) -> Option<(Status, u32)> {
    if b.len() < CSW_LEN || u32_le(b, 0) != CSW_SIG || u32_le(b, 4) != tag {
        return None;
    }
    let status = match b[12] {
        0 => Status::Passed,
        1 => Status::Failed,
        2 => Status::PhaseError,
        _ => return None,
    };
    Some((status, u32_le(b, 8)))
}

// ---- SCSI commands (all of them read) ----

pub const INQUIRY_LEN: u16 = 36;
pub const SENSE_LEN: u16 = 18;

pub fn test_unit_ready() -> [u8; 6] {
    [0x00, 0, 0, 0, 0, 0]
}

pub fn request_sense() -> [u8; 6] {
    [0x03, 0, 0, 0, SENSE_LEN as u8, 0]
}

pub fn inquiry() -> [u8; 6] {
    [0x12, 0, 0, 0, INQUIRY_LEN as u8, 0]
}

/// READ CAPACITY(10): 8 bytes back.
pub fn read_capacity10() -> [u8; 10] {
    [0x25, 0, 0, 0, 0, 0, 0, 0, 0, 0]
}

/// READ CAPACITY(16) (SERVICE ACTION IN(16)): 32 bytes back.
pub fn read_capacity16() -> [u8; 16] {
    let mut c = [0u8; 16];
    c[0] = 0x9e;
    c[1] = 0x10;
    c[13] = 32;
    c
}

/// READ of `blocks` blocks at `lba`: READ(10) where it reaches, else
/// READ(16). Returns the CDB and its length.
pub fn read(lba: u64, blocks: u16) -> ([u8; 16], usize) {
    let mut c = [0u8; 16];
    if lba + blocks as u64 <= u32::MAX as u64 {
        c[0] = 0x28;
        c[2..6].copy_from_slice(&(lba as u32).to_be_bytes());
        c[7..9].copy_from_slice(&blocks.to_be_bytes());
        (c, 10)
    } else {
        c[0] = 0x88;
        c[2..10].copy_from_slice(&lba.to_be_bytes());
        c[10..14].copy_from_slice(&(blocks as u32).to_be_bytes());
        (c, 16)
    }
}

pub struct Inquiry {
    /// The peripheral device type (0: a disk).
    pub device_type: u8,
    pub removable: bool,
    pub vendor: String,
    pub product: String,
    pub revision: String,
}

pub fn parse_inquiry(b: &[u8]) -> Option<Inquiry> {
    if b.len() < INQUIRY_LEN as usize {
        return None;
    }
    let text = |r: core::ops::Range<usize>| {
        let s: String = b[r].iter().map(|&c| if (0x20..0x7f).contains(&c) { c as char } else { '?' }).collect();
        String::from(s.trim_end())
    };
    Some(Inquiry {
        device_type: b[0] & 0x1f,
        removable: b[1] & 0x80 != 0,
        vendor: text(8..16),
        product: text(16..32),
        revision: text(32..36),
    })
}

/// READ CAPACITY(10)'s answer: (number of blocks, block size). A last LBA
/// of 0xffffffff means "too big, ask READ CAPACITY(16)": None then.
pub fn parse_capacity10(b: &[u8]) -> Option<(u64, u32)> {
    if b.len() < 8 {
        return None;
    }
    let last = u32::from_be_bytes(b[0..4].try_into().unwrap());
    if last == u32::MAX {
        return None;
    }
    Some((last as u64 + 1, u32::from_be_bytes(b[4..8].try_into().unwrap())))
}

pub fn parse_capacity16(b: &[u8]) -> Option<(u64, u32)> {
    if b.len() < 12 {
        return None;
    }
    let last = u64::from_be_bytes(b[0..8].try_into().unwrap());
    Some((last.checked_add(1)?, u32::from_be_bytes(b[8..12].try_into().unwrap())))
}

/// Fixed-format sense data: (sense key, ASC, ASCQ).
pub fn parse_sense(b: &[u8]) -> Option<(u8, u8, u8)> {
    if b.len() < 14 || b[0] & 0x7e != 0x70 {
        return None;
    }
    Some((b[2] & 0xf, b[12], b[13]))
}

/// What a partition's first block says it holds (for the log).
pub fn fs_name(boot: &[u8]) -> &'static str {
    if boot.len() < 512 {
        return "?";
    }
    match &boot[3..11] {
        b"EXFAT   " => return "exFAT",
        b"NTFS    " => return "NTFS",
        _ => {}
    }
    if &boot[82..90] == b"FAT32   " {
        return "FAT32";
    }
    if &boot[54..59] == b"FAT16" || &boot[54..59] == b"FAT12" {
        return "FAT12/16";
    }
    if boot[510..512] == [0x55, 0xaa] { "unknown (boot signature)" } else { "unknown" }
}

/// The four primary entries of an MBR: (type, first LBA, blocks), empty
/// ones left out. None without the boot signature.
pub fn mbr_entries(b: &[u8]) -> Option<alloc::vec::Vec<(u8, u64, u64)>> {
    if b.len() < 512 || b[510..512] != [0x55, 0xaa] {
        return None;
    }
    Some(
        b[446..510]
            .chunks(16)
            .filter(|e| e[4] != 0)
            .map(|e| (e[4], u32_le(e, 8) as u64, u32_le(e, 12) as u64))
            .collect(),
    )
}

fn u32_le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cbw_layout() {
        let b = cbw(0x1234_5678, 512, true, 0, &read(7, 1).0[..10]);
        assert_eq!(&b[0..4], b"USBC");
        assert_eq!(u32_le(&b, 4), 0x1234_5678);
        assert_eq!(u32_le(&b, 8), 512);
        assert_eq!((b[12], b[13], b[14]), (0x80, 0, 10));
        assert_eq!(&b[15..25], &[0x28, 0, 0, 0, 0, 7, 0, 0, 1, 0]);
    }

    #[test]
    fn read_switches_to_16_bytes_past_2_tib() {
        let (c, n) = read(u32::MAX as u64 - 1, 1);
        assert_eq!((c[0], n), (0x28, 10));
        let (c, n) = read(u32::MAX as u64, 8);
        assert_eq!((c[0], n), (0x88, 16));
        assert_eq!(&c[2..10], &(u32::MAX as u64).to_be_bytes());
        assert_eq!(&c[10..14], &8u32.to_be_bytes());
    }

    #[test]
    fn csw_checks_signature_and_tag() {
        let mut b = [0u8; 13];
        b[0..4].copy_from_slice(b"USBS");
        b[4..8].copy_from_slice(&9u32.to_le_bytes());
        b[8..12].copy_from_slice(&100u32.to_le_bytes());
        assert_eq!(parse_csw(&b, 9), Some((Status::Passed, 100)));
        assert_eq!(parse_csw(&b, 8), None);
        b[12] = 2;
        assert_eq!(parse_csw(&b, 9), Some((Status::PhaseError, 100)));
        b[12] = 7;
        assert_eq!(parse_csw(&b, 9), None);
        b[12] = 1;
        b[0] = b'X';
        assert_eq!(parse_csw(&b, 9), None);
    }

    #[test]
    fn answers() {
        // A 500 GB disk: last LBA 976773167, 512-byte blocks.
        let mut c = [0u8; 8];
        c[0..4].copy_from_slice(&976_773_167u32.to_be_bytes());
        c[4..8].copy_from_slice(&512u32.to_be_bytes());
        assert_eq!(parse_capacity10(&c), Some((976_773_168, 512)));
        c[0..4].fill(0xff);
        assert_eq!(parse_capacity10(&c), None);
        let mut i = [b' '; 36];
        i[0] = 0;
        i[1] = 0;
        i[8..15].copy_from_slice(b"Samsung");
        i[16..32].copy_from_slice(b"Portable SSD T5 ");
        i[32..33].copy_from_slice(b"0");
        let q = parse_inquiry(&i).unwrap();
        assert_eq!((q.device_type, q.removable), (0, false));
        assert_eq!((q.vendor.as_str(), q.product.as_str(), q.revision.as_str()), ("Samsung", "Portable SSD T5", "0"));
        let mut s = [0u8; 18];
        s[0] = 0x70;
        s[2] = 0x06; // unit attention
        s[12] = 0x28;
        assert_eq!(parse_sense(&s), Some((6, 0x28, 0)));
    }

    #[test]
    fn partitions_and_filesystems() {
        let mut mbr = [0u8; 512];
        mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
        mbr[446 + 4] = 0x07;
        mbr[446 + 8..446 + 12].copy_from_slice(&2048u32.to_le_bytes());
        mbr[446 + 12..446 + 16].copy_from_slice(&1000u32.to_le_bytes());
        assert_eq!(mbr_entries(&mbr), Some(alloc::vec![(0x07, 2048, 1000)]));
        let mut boot = [0u8; 512];
        boot[3..11].copy_from_slice(b"EXFAT   ");
        assert_eq!(fs_name(&boot), "exFAT");
        boot[3..11].copy_from_slice(b"MSDOS5.0");
        boot[82..90].copy_from_slice(b"FAT32   ");
        assert_eq!(fs_name(&boot), "FAT32");
    }
}
