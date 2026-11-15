//! The system copies and the state ring, for the boot stage and the
//! storage server alike (over [`Flash`], so tested on the host):
//!
//! - reading and writing a region's good blocks in order ([`read`],
//!   [`write`], [`verify`]);
//! - the state ring: the newest record ([`newest_state`]) and a new one
//!   ([`append_state`]);
//! - writing a system copy from its parts, the header page last
//!   ([`write_copy`]);
//! - which copy to start, and the record that says so ([`plan`]).
//!
//! **Which copy starts.** The state record names the active copy, a copy
//! on trial (just installed) and a count of starts. The boot stage writes
//! a record at every start, and touchos writes one when the copy it runs
//! has proven itself (init's health check): [`confirmed`].
//! - A copy on trial gets [`TRIAL_STARTS`] starts; if none of them is
//!   confirmed, the stage goes back to the active copy.
//! - The active copy that starts [`MAX_STARTS`] times in a row without
//!   being confirmed (a hang or a crash loop, ended by the watchdog) hands
//!   over to the other copy, if that one has a valid header.
//!
//! Nothing here erases or writes below [`FIRST_WRITABLE`].

use crate::image::{Part, SysHeader, State, crc32};
use crate::layout::{self, BadBlocks, FIRST_WRITABLE, Region};
use crate::nfc::{self, BLOCK, PAGE, PAGES_PER_BLOCK};
use crate::store::Flash;

/// Starts a copy on trial gets before the active copy takes over again.
pub const TRIAL_STARTS: u8 = 3;
/// Starts in a row without being confirmed after which the active copy
/// hands over to the other one.
pub const MAX_STARTS: u8 = 4;

fn erase(f: &mut impl Flash, block: u32) -> Result<(), nfc::Error> {
    if block < FIRST_WRITABLE {
        return Err(nfc::Error::Range);
    }
    f.erase_block(block)
}

/// Read `out.len()` bytes from byte `offset` of `region` (its good blocks
/// in order).
pub fn read(f: &mut impl Flash, bad: &BadBlocks, region: Region, mut offset: usize, out: &mut [u8]) -> Result<(), nfc::Error> {
    let mut page = [0u8; PAGE];
    let mut done = 0;
    while done < out.len() {
        let (block, off) = region.locate(bad, offset).ok_or(nfc::Error::Range)?;
        let (p, in_page) = (off / PAGE, off % PAGE);
        f.read_page(block * PAGES_PER_BLOCK + p as u32, &mut page)?;
        let n = (PAGE - in_page).min(out.len() - done);
        out[done..done + n].copy_from_slice(&page[in_page..in_page + n]);
        done += n;
        offset += n;
    }
    Ok(())
}

/// Writes data into a region from its start, page by page, erasing each
/// good block as it gets there.
pub struct Writer<'a> {
    region: Region,
    bad: &'a BadBlocks,
    /// Bytes written so far (always whole pages).
    at: usize,
}

impl<'a> Writer<'a> {
    pub fn new(region: Region, bad: &'a BadBlocks) -> Writer<'a> {
        Writer { region, bad, at: 0 }
    }

    /// Where the next byte goes (from the region's start).
    pub fn offset(&self) -> usize {
        self.at
    }

    /// Append `data`, padded with 0xff to whole pages.
    pub fn put(&mut self, f: &mut impl Flash, data: &[u8]) -> Result<(), nfc::Error> {
        let mut page = [0xffu8; PAGE];
        for chunk in data.chunks(PAGE) {
            let (block, off) = self.region.locate(self.bad, self.at).ok_or(nfc::Error::Range)?;
            if off == 0 {
                erase(f, block)?;
            }
            page.fill(0xff);
            page[..chunk.len()].copy_from_slice(chunk);
            f.program_page(block * PAGES_PER_BLOCK + (off / PAGE) as u32, &page)?;
            self.at += PAGE;
        }
        Ok(())
    }
}

/// Erase the blocks `data` needs in `region` (from its start) and write it.
pub fn write(f: &mut impl Flash, bad: &BadBlocks, region: Region, data: &[u8]) -> Result<(), nfc::Error> {
    if data.len() > region.capacity(bad) {
        return Err(nfc::Error::Range);
    }
    Writer::new(region, bad).put(f, data)
}

/// Read `data.len()` bytes back from byte `offset` of `region` and
/// compare.
pub fn verify(f: &mut impl Flash, bad: &BadBlocks, region: Region, offset: usize, data: &[u8]) -> Result<bool, nfc::Error> {
    let mut page = [0u8; PAGE];
    for (i, chunk) in data.chunks(PAGE).enumerate() {
        read(f, bad, region, offset + i * PAGE, &mut page[..chunk.len()])?;
        if page[..chunk.len()] != *chunk {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The first page of `region`'s first good block.
pub fn first_page(bad: &BadBlocks, region: Region) -> Option<u32> {
    region.good(bad).next().map(|b| b * PAGES_PER_BLOCK)
}

/// The newest state record and its page.
pub fn newest_state(f: &mut impl Flash, bad: &BadBlocks) -> Option<(State, u32)> {
    let mut page = [0u8; PAGE];
    let mut best: Option<(State, u32)> = None;
    for b in layout::STATE.good(bad) {
        for p in 0..PAGES_PER_BLOCK {
            let n = b * PAGES_PER_BLOCK + p;
            if f.read_page(n, &mut page).is_err() {
                continue;
            }
            if let Some(s) = State::decode(&page)
                && best.is_none_or(|(x, _)| s.seq > x.seq)
            {
                best = Some((s, n));
            }
        }
    }
    best
}

/// The good block after `b` in `ring`, wrapping.
pub fn next_block(bad: &BadBlocks, ring: Region, b: u32) -> u32 {
    let first = ring.good(bad).next().unwrap_or(ring.first);
    ring.good(bad).find(|&x| x > b).unwrap_or(first)
}

/// Write `s` (its `seq` set to follow the newest) in the next page of the
/// ring, erasing the next block when the newest record ends one. Returns
/// the record as written.
pub fn append_state(f: &mut impl Flash, bad: &BadBlocks, mut s: State) -> Result<State, nfc::Error> {
    let newest = newest_state(f, bad);
    s.seq = newest.map_or(1, |(x, _)| x.seq.wrapping_add(1));
    let target = match newest {
        Some((_, page)) if (page + 1) % PAGES_PER_BLOCK != 0 => page + 1,
        Some((_, page)) => {
            let b = next_block(bad, layout::STATE, page / PAGES_PER_BLOCK);
            erase(f, b)?;
            b * PAGES_PER_BLOCK
        }
        None => {
            let b = layout::STATE.good(bad).next().ok_or(nfc::Error::Range)?;
            erase(f, b)?;
            b * PAGES_PER_BLOCK
        }
    };
    let mut page = [0xffu8; PAGE];
    page[..crate::image::STATE_LEN].copy_from_slice(&s.encode());
    f.program_page(target, &page)?;
    Ok(s)
}

/// The header of copy `slot`, if it has a valid one.
pub fn header(f: &mut impl Flash, bad: &BadBlocks, slot: u8) -> Option<SysHeader> {
    let region = layout::slot(slot)?;
    let mut page = [0u8; PAGE];
    f.read_page(first_page(bad, region)?, &mut page).ok()?;
    SysHeader::decode(&page)
}

/// Write system copy `slot`: its header block is erased first (the copy
/// is invalid from then on), then the kernel, initrd and boot info go
/// from the next good block on, page-aligned, each read back; the header
/// page comes last. As `cargo xtask flash` lays a copy out.
pub fn write_copy(f: &mut impl Flash, bad: &BadBlocks, slot: u8, build: &str, kernel: &[u8], initrd: &[u8], bootinfo: &[u8]) -> Result<SysHeader, nfc::Error> {
    let region = layout::slot(slot).ok_or(nfc::Error::Range)?;
    let mut blocks = region.good(bad);
    let header_block = blocks.next().ok_or(nfc::Error::Range)?;
    let data_first = blocks.next().ok_or(nfc::Error::Range)?;
    let pages = |n: usize| n.next_multiple_of(PAGE);
    if BLOCK + pages(kernel.len()) + pages(initrd.len()) + pages(bootinfo.len()) > region.capacity(bad) {
        return Err(nfc::Error::Range);
    }
    erase(f, header_block)?;
    let data = Region { first: data_first, end: region.end };
    let mut w = Writer::new(data, bad);
    let mut parts = [Part::default(); 3];
    for (i, d) in [kernel, initrd, bootinfo].into_iter().enumerate() {
        let at = w.offset();
        w.put(f, d)?;
        if !verify(f, bad, data, at, d)? {
            return Err(nfc::Error::Verify);
        }
        // Offsets count from the copy's start: the header block first.
        parts[i] = Part { offset: (BLOCK + at) as u32, len: d.len() as u32, crc: crc32(0, d) };
    }
    let mut id = [0u8; 48];
    let n = build.len().min(47);
    id[..n].copy_from_slice(&build.as_bytes()[..n]);
    let h = SysHeader { build: id, kernel: parts[0], initrd: parts[1], bootinfo: parts[2] };
    let mut page = [0xffu8; PAGE];
    page[..crate::image::SYS_HEADER_LEN].copy_from_slice(&h.encode());
    f.program_page(header_block * PAGES_PER_BLOCK, &page)?;
    if header(f, bad, slot) != Some(h) {
        return Err(nfc::Error::Verify);
    }
    Ok(h)
}

/// Why the stage starts the copy it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The active copy, as usual.
    Active,
    /// The copy on trial (start n of [`TRIAL_STARTS`]).
    Trial,
    /// The copy on trial wasn't confirmed in its starts: back to the
    /// active one.
    TrialFailed,
    /// The active copy wasn't confirmed in [`MAX_STARTS`] starts: the
    /// other one takes over.
    FellBack,
    /// No state record yet: copy A.
    NoRecord,
}

/// What to start: the copy, the record to write before starting it, and
/// why. `valid(slot)`: that copy has a valid header (the stage then
/// checks its CRCs while loading it; see [`loaded_instead`]).
pub fn plan(s: Option<State>, valid: impl Fn(u8) -> bool) -> (u8, State, Reason) {
    let Some(s) = s else {
        return (b'A', State { seq: 0, active: b'A', trial: 0, starts: 1 }, Reason::NoRecord);
    };
    let other = other(s.active);
    if s.trial != 0 {
        if s.starts < TRIAL_STARTS {
            return (s.trial, State { starts: s.starts + 1, ..s }, Reason::Trial);
        }
        return (s.active, State { trial: 0, starts: 1, ..s }, Reason::TrialFailed);
    }
    if s.starts >= MAX_STARTS && valid(other) {
        return (other, State { active: other, trial: 0, starts: 1, ..s }, Reason::FellBack);
    }
    (s.active, State { starts: s.starts.saturating_add(1), ..s }, Reason::Active)
}

/// The planned copy didn't load (a CRC failed), and `slot` did instead:
/// the record that says so (it is the active copy now, nothing on trial).
pub fn loaded_instead(planned: State, slot: u8) -> State {
    State { active: slot, trial: 0, starts: 1, ..planned }
}

/// The record that confirms `slot` (touchos runs it, and it is healthy):
/// None if the newest record says so already.
pub fn confirmed(newest: Option<State>, slot: u8) -> Option<State> {
    let want = State { seq: 0, active: slot, trial: 0, starts: 0 };
    match newest {
        Some(s) if State { seq: 0, ..s } == want => None,
        _ => Some(want),
    }
}

/// The other copy's letter.
pub fn other(slot: u8) -> u8 {
    if slot == b'A' { b'B' } else { b'A' }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    /// A chip: programming only clears bits, erasing sets a block to 0xff.
    struct Sim {
        pages: Vec<[u8; PAGE]>,
    }

    impl Sim {
        fn new() -> Sim {
            Sim { pages: vec![[0x5a; PAGE]; (nfc::BLOCKS * PAGES_PER_BLOCK) as usize] }
        }
    }

    impl Flash for Sim {
        fn read_page(&mut self, page: u32, data: &mut [u8; PAGE]) -> Result<(), nfc::Error> {
            *data = self.pages[page as usize];
            Ok(())
        }
        fn program_page(&mut self, page: u32, data: &[u8; PAGE]) -> Result<(), nfc::Error> {
            assert!(page / PAGES_PER_BLOCK >= FIRST_WRITABLE);
            for (d, s) in self.pages[page as usize].iter_mut().zip(data) {
                *d &= s;
            }
            Ok(())
        }
        fn erase_block(&mut self, block: u32) -> Result<(), nfc::Error> {
            assert!(block >= FIRST_WRITABLE);
            for p in 0..PAGES_PER_BLOCK {
                self.pages[(block * PAGES_PER_BLOCK + p) as usize] = [0xff; PAGE];
            }
            Ok(())
        }
    }

    fn toon() -> BadBlocks {
        let mut b = BadBlocks::default();
        for x in [25, 59, 377] {
            b.mark(x);
        }
        b
    }

    #[test]
    fn a_copy_is_written_header_last_and_reads_back() {
        let (mut f, bad) = (Sim::new(), toon());
        let kernel: Vec<u8> = (0..700_000u32).map(|i| (i * 7) as u8).collect();
        let initrd: Vec<u8> = (0..3_000_000u32).map(|i| (i * 13 + 1) as u8).collect();
        let bootinfo = vec![9u8; 536];
        // Copy A holds bad block 377: the data steps over it.
        let h = write_copy(&mut f, &bad, b'A', "abc123", &kernel, &initrd, &bootinfo).unwrap();
        assert_eq!(header(&mut f, &bad, b'A'), Some(h));
        assert_eq!(h.build_str(), "abc123");
        let region = layout::SLOT_A;
        for (part, want) in [(h.kernel, &kernel), (h.initrd, &initrd), (h.bootinfo, &bootinfo)] {
            assert_eq!(part.offset as usize % PAGE, 0);
            let mut got = vec![0u8; part.len as usize];
            read(&mut f, &bad, region, part.offset as usize, &mut got).unwrap();
            assert_eq!(&got, want);
            assert_eq!(crc32(0, &got), part.crc);
        }
        // Too big for a copy: refused before anything is erased.
        let huge = vec![0u8; 41 << 20];
        assert_eq!(write_copy(&mut f, &bad, b'B', "x", &huge, &[], &[]), Err(nfc::Error::Range));
        assert_eq!(header(&mut f, &bad, b'B'), None); // (it never had one)
    }

    #[test]
    fn state_records_fill_the_ring_and_wrap() {
        let (mut f, bad) = (Sim::new(), toon());
        assert_eq!(newest_state(&mut f, &bad), None);
        for i in 0..(4 * PAGES_PER_BLOCK + 10) {
            let s = append_state(&mut f, &bad, State { seq: 0, active: b'A', trial: 0, starts: (i % 200) as u8 }).unwrap();
            assert_eq!(s.seq, i + 1);
            assert_eq!(newest_state(&mut f, &bad).map(|(s, _)| s), Some(s));
        }
    }

    #[test]
    fn the_stage_plans_starts_trials_and_fallbacks() {
        let both = |_: u8| true;
        // No record: copy A.
        assert_eq!(plan(None, both).0, b'A');
        // A new copy B on trial: three starts, then back to A.
        let mut s = State { seq: 5, active: b'A', trial: b'B', starts: 0 };
        for n in 1..=TRIAL_STARTS {
            let (slot, next, why) = plan(Some(s), both);
            assert_eq!((slot, why, next.starts), (b'B', Reason::Trial, n));
            s = next;
        }
        let (slot, next, why) = plan(Some(s), both);
        assert_eq!((slot, why, next.trial, next.active), (b'A', Reason::TrialFailed, 0, b'A'));
        // Confirmed on its second start instead: B is the active copy.
        let c = confirmed(Some(State { trial: b'B', starts: 2, ..s }), b'B').unwrap();
        assert_eq!((c.active, c.trial, c.starts), (b'B', 0, 0));
        assert_eq!(confirmed(Some(State { seq: 9, ..c }), b'B'), None);
        // The active copy keeps failing: after MAX_STARTS the other takes over...
        let mut s = State { seq: 1, active: b'A', trial: 0, starts: 0 };
        for _ in 0..MAX_STARTS {
            let (slot, next, why) = plan(Some(s), both);
            assert_eq!((slot, why), (b'A', Reason::Active));
            s = next;
        }
        let (slot, next, why) = plan(Some(s), both);
        assert_eq!((slot, why, next.active, next.starts), (b'B', Reason::FellBack, b'B', 1));
        // ...unless it has no valid header: A carries on, counting.
        let (slot, next, why) = plan(Some(s), |x| x == b'A');
        assert_eq!((slot, why, next.starts), (b'A', Reason::Active, MAX_STARTS + 1));
        // A copy that didn't load: the one that did is the active one.
        let l = loaded_instead(State { seq: 3, active: b'A', trial: b'B', starts: 1 }, b'A');
        assert_eq!((l.active, l.trial, l.starts), (b'A', 0, 1));
    }
}
