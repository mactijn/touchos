//! touchos.toml on the NAND: the config ring ([`crate::layout::CONFIG`],
//! one record per block, libs/nand's `image::Config`).
//!
//! The storage server's REPLACE becomes records:
//! - **current** (touchos.toml): the newest `Current` record;
//! - **backup** (TOUCHOS.BAK): the newest `Current` record before it that
//!   isn't known to be broken (no `Broken` record has its CRC);
//! - **broken** (TOUCHOS.BAD): the newest `Broken` record.
//!
//! A save goes into the ring's next block (after the newest record's,
//! wrapping), which holds the oldest record: the 7 before it stay. A
//! record is erased-then-programmed in one block, and read back before
//! the save counts; a power cut leaves either the old newest record or a
//! new one that fails its CRC (and is passed over).
//!
//! [`Flash`] is what the chip does, so this is tested on the host with a
//! simulated chip.

use crate::image::{CONFIG_HEADER_LEN, CONFIG_MAX, Config, ConfigKind, crc32};
use crate::layout::{BadBlocks, Region};
use crate::nfc::{self, Nfc, PAGE, PAGES_PER_BLOCK};

/// The chip, as the store needs it.
pub trait Flash {
    fn read_page(&mut self, page: u32, data: &mut [u8; PAGE]) -> Result<(), nfc::Error>;
    fn program_page(&mut self, page: u32, data: &[u8; PAGE]) -> Result<(), nfc::Error>;
    fn erase_block(&mut self, block: u32) -> Result<(), nfc::Error>;
}

impl Flash for &Nfc {
    fn read_page(&mut self, page: u32, data: &mut [u8; PAGE]) -> Result<(), nfc::Error> {
        Nfc::read_page(self, page, data, None).map(|_| ())
    }
    fn program_page(&mut self, page: u32, data: &[u8; PAGE]) -> Result<(), nfc::Error> {
        Nfc::program_page(self, page, data)
    }
    fn erase_block(&mut self, block: u32) -> Result<(), nfc::Error> {
        Nfc::erase_block(self, block)
    }
}

/// The ring's records: (block, header), in block order.
pub struct Records {
    list: [(u32, Config); 16],
    n: usize,
}

impl Records {
    pub fn iter(&self) -> impl Iterator<Item = &(u32, Config)> {
        self.list[..self.n].iter()
    }

    fn newest(&self, f: impl Fn(&Config) -> bool) -> Option<(u32, Config)> {
        self.iter().filter(|(_, c)| f(c)).max_by_key(|(_, c)| c.seq).copied()
    }

    pub fn current(&self) -> Option<(u32, Config)> {
        self.newest(|c| c.kind == ConfigKind::Current)
    }

    pub fn backup(&self) -> Option<(u32, Config)> {
        let cur = self.current()?;
        let broken = |crc: u32| self.iter().any(|(_, c)| c.kind == ConfigKind::Broken && c.crc == crc);
        self.newest(|c| c.kind == ConfigKind::Current && c.seq < cur.1.seq && !broken(c.crc))
    }

    pub fn broken(&self) -> Option<(u32, Config)> {
        self.newest(|c| c.kind == ConfigKind::Broken)
    }

    /// The next sequence number.
    pub fn next_seq(&self) -> u32 {
        self.iter().map(|(_, c)| c.seq).max().map_or(1, |s| s + 1)
    }
}

/// Read every block's header (a block whose first page doesn't read, or
/// holds no record, is skipped).
pub fn scan(flash: &mut impl Flash, bad: &BadBlocks, ring: Region) -> Records {
    let mut r = Records { list: [(0, Config { seq: 0, kind: ConfigKind::Current, len: 0, crc: 0 }); 16], n: 0 };
    let mut page = [0u8; PAGE];
    for b in ring.good(bad) {
        if r.n == r.list.len() {
            break;
        }
        if flash.read_page(b * PAGES_PER_BLOCK, &mut page).is_ok()
            && let Some(c) = Config::decode(&page)
        {
            r.list[r.n] = (b, c);
            r.n += 1;
        }
    }
    r
}

/// Read record `c` of `block` into `out` (at least `c.len` bytes); false
/// if it fails its CRC.
pub fn read(flash: &mut impl Flash, block: u32, c: &Config, out: &mut [u8]) -> Result<bool, nfc::Error> {
    let len = c.len as usize;
    if out.len() < len {
        return Err(nfc::Error::Range);
    }
    let mut page = [0u8; PAGE];
    let mut at = 0; // bytes of the record (header included) read so far
    let mut p = 0;
    while at < CONFIG_HEADER_LEN + len {
        flash.read_page(block * PAGES_PER_BLOCK + p, &mut page)?;
        let from = if p == 0 { CONFIG_HEADER_LEN } else { 0 };
        let n = (PAGE - from).min(CONFIG_HEADER_LEN + len - at - from);
        let dst = at + from - CONFIG_HEADER_LEN;
        out[dst..dst + n].copy_from_slice(&page[from..from + n]);
        at += from + n;
        p += 1;
    }
    Ok(crc32(0, &out[..len]) == c.crc)
}

/// Save `data` as a new record of `kind` in the ring's next block, and
/// read it back. Returns the block it went into.
pub fn append(flash: &mut impl Flash, bad: &BadBlocks, ring: Region, kind: ConfigKind, data: &[u8]) -> Result<u32, nfc::Error> {
    if data.len() > CONFIG_MAX {
        return Err(nfc::Error::Range);
    }
    let recs = scan(flash, bad, ring);
    let first = ring.good(bad).next().ok_or(nfc::Error::Range)?;
    let block = match recs.newest(|_| true) {
        Some((b, _)) => ring.good(bad).find(|&x| x > b).unwrap_or(first),
        None => first,
    };
    let h = Config { seq: recs.next_seq(), kind, len: data.len() as u32, crc: crc32(0, data) };
    flash.erase_block(block)?;
    let mut page = [0xffu8; PAGE];
    let total = CONFIG_HEADER_LEN + data.len();
    let mut at = 0;
    let mut p = 0;
    while at < total {
        page.fill(0xff);
        let mut i = 0;
        if p == 0 {
            page[..CONFIG_HEADER_LEN].copy_from_slice(&h.encode());
            i = CONFIG_HEADER_LEN;
            at = CONFIG_HEADER_LEN;
        }
        let n = (PAGE - i).min(total - at);
        let src = at - CONFIG_HEADER_LEN;
        page[i..i + n].copy_from_slice(&data[src..src + n]);
        flash.program_page(block * PAGES_PER_BLOCK + p, &page)?;
        at += n;
        p += 1;
    }
    // Read back: the header, then the data's CRC.
    let mut back = [0u8; PAGE];
    flash.read_page(block * PAGES_PER_BLOCK, &mut back)?;
    if Config::decode(&back) != Some(h) {
        return Err(nfc::Error::Failed);
    }
    let mut crc = 0;
    let mut left = data.len();
    let mut p = 0;
    while left > 0 {
        flash.read_page(block * PAGES_PER_BLOCK + p, &mut back)?;
        let from = if p == 0 { CONFIG_HEADER_LEN } else { 0 };
        let n = (PAGE - from).min(left);
        crc = crc32(crc, &back[from..from + n]);
        left -= n;
        p += 1;
    }
    if crc != h.crc {
        return Err(nfc::Error::Failed);
    }
    Ok(block)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::layout::CONFIG;
    use std::vec;
    use std::vec::Vec;

    /// A chip: programming only clears bits, erasing sets a block to 0xff.
    struct Sim {
        pages: Vec<[u8; PAGE]>,
        erases: u32,
    }

    impl Sim {
        fn new() -> Sim {
            Sim { pages: vec![[0xff; PAGE]; (nfc::BLOCKS * PAGES_PER_BLOCK) as usize], erases: 0 }
        }
    }

    impl Flash for Sim {
        fn read_page(&mut self, page: u32, data: &mut [u8; PAGE]) -> Result<(), nfc::Error> {
            *data = self.pages[page as usize];
            Ok(())
        }
        fn program_page(&mut self, page: u32, data: &[u8; PAGE]) -> Result<(), nfc::Error> {
            for (d, s) in self.pages[page as usize].iter_mut().zip(data) {
                *d &= s;
            }
            Ok(())
        }
        fn erase_block(&mut self, block: u32) -> Result<(), nfc::Error> {
            assert!(CONFIG.contains(block), "erase outside the config ring");
            self.erases += 1;
            for p in 0..PAGES_PER_BLOCK {
                self.pages[(block * PAGES_PER_BLOCK + p) as usize] = [0xff; PAGE];
            }
            Ok(())
        }
    }

    fn text(f: &mut Sim, r: Option<(u32, Config)>) -> Option<Vec<u8>> {
        let (b, c) = r?;
        let mut out = vec![0u8; c.len as usize];
        read(f, b, &c, &mut out).unwrap().then_some(out)
    }

    #[test]
    fn saves_backups_and_broken_files() {
        let mut f = Sim::new();
        let mut bad = BadBlocks::default();
        bad.mark(78); // in the ring: stepped over
        let ring = CONFIG;
        let r = scan(&mut f, &bad, ring);
        assert!(r.current().is_none() && r.backup().is_none());

        append(&mut f, &bad, ring, ConfigKind::Current, b"one").unwrap();
        let r = scan(&mut f, &bad, ring);
        assert_eq!(text(&mut f, r.current()).unwrap(), b"one");
        assert!(r.backup().is_none());

        // A config over several pages.
        let big: Vec<u8> = (0..10_000u32).map(|i| i as u8).collect();
        append(&mut f, &bad, ring, ConfigKind::Current, &big).unwrap();
        let r = scan(&mut f, &bad, ring);
        assert_eq!(text(&mut f, r.current()).unwrap(), big);
        assert_eq!(text(&mut f, r.backup()).unwrap(), b"one");

        // "big" didn't parse: it is kept aside, "one" comes back.
        append(&mut f, &bad, ring, ConfigKind::Broken, &big).unwrap();
        append(&mut f, &bad, ring, ConfigKind::Current, b"one").unwrap();
        let r = scan(&mut f, &bad, ring);
        assert_eq!(text(&mut f, r.current()).unwrap(), b"one");
        assert_eq!(text(&mut f, r.broken()).unwrap(), big);
        assert_eq!(text(&mut f, r.backup()).unwrap(), b"one"); // not "big"

        // Round the ring several times: the newest stays, bad block 78 is
        // never used, and the sequence keeps rising.
        for i in 0..20u32 {
            let b = append(&mut f, &bad, ring, ConfigKind::Current, &i.to_le_bytes()).unwrap();
            assert_ne!(b, 78);
        }
        let r = scan(&mut f, &bad, ring);
        assert_eq!(text(&mut f, r.current()).unwrap(), 19u32.to_le_bytes());
        assert_eq!(text(&mut f, r.backup()).unwrap(), 18u32.to_le_bytes());
        assert_eq!(r.iter().count(), 7); // 8 blocks, one bad
        assert_eq!(r.next_seq(), 25);
    }

    #[test]
    fn a_damaged_newest_record_is_passed_over() {
        let mut f = Sim::new();
        let bad = BadBlocks::default();
        append(&mut f, &bad, CONFIG, ConfigKind::Current, b"good").unwrap();
        let b = append(&mut f, &bad, CONFIG, ConfigKind::Current, b"newer").unwrap();
        // A bit flips in the data (the header still reads): the CRC says so.
        f.pages[(b * PAGES_PER_BLOCK) as usize][CONFIG_HEADER_LEN] ^= 1;
        let r = scan(&mut f, &bad, CONFIG);
        let (blk, c) = r.current().unwrap();
        let mut out = vec![0u8; c.len as usize];
        assert!(!read(&mut f, blk, &c, &mut out).unwrap());
        // A power cut after the erase: no header, the older record wins.
        f.erase_block(b).unwrap();
        let r = scan(&mut f, &bad, CONFIG);
        assert_eq!(text(&mut f, r.current()).unwrap(), b"good");
    }
}
