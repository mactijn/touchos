//! Where things go on the Toon's NAND (1024 blocks of 128 KiB), and how
//! bad blocks are stepped over. Pure, tested on the host.
//!
//! | Blocks | Bytes | What |
//! |---|---|---|
//! | 0-23 | 0x000000-0x2fffff | U-Boot, its env, the splash: **never touched** |
//! | 24-71 | 0x300000-0x8fffff | the boot image U-Boot loads (`boot_nand` reads the stock `kernel` partition: 3 MiB of good blocks from block 24 on) |
//! | 72-1023 | 0x900000-0x7ffffff | touchos's own area (`System`) |
//!
//! Data that spans blocks goes into the good blocks of its region in
//! order, skipping bad ones, exactly as U-Boot's `nand read` does; so a
//! region's capacity is its good blocks.

use crate::nfc::{BLOCK, BLOCKS};

/// The first block touchos may erase or write (0x300000): everything
/// before it (U-Boot, its environment, the splash image) stays as it is.
pub const FIRST_WRITABLE: u32 = 24;
/// The boot image's region: blocks 24..72 (the stock `kernel` and
/// `kernel-backup` partitions). U-Boot reads 3 MiB of good blocks from
/// block 24, so the image may be up to 3 MiB.
pub const BOOT: Region = Region { first: 24, end: 72 };
/// What U-Boot's `nand read ${loadaddr} kernel` reads (the stock
/// partition's size): the boot image's limit.
pub const BOOT_MAX: usize = 3 << 20;
/// touchos's own area: blocks 72..1024 (the stock rootfs partition).
pub const SYSTEM: Region = Region { first: 72, end: BLOCKS };

/// touchos's area, in order: the state ring (which copy is active; one
/// page per record), the config ring (one record per block), then the
/// two system copies (40 MiB each, bad blocks included), then nothing yet.
pub const STATE: Region = Region { first: 72, end: 76 };
pub const CONFIG: Region = Region { first: 76, end: 84 };
pub const SLOT_A: Region = Region { first: 84, end: 404 };
pub const SLOT_B: Region = Region { first: 404, end: 724 };

/// A system copy's region, by its letter.
pub fn slot(letter: u8) -> Option<Region> {
    match letter {
        b'A' => Some(SLOT_A),
        b'B' => Some(SLOT_B),
        _ => None,
    }
}

/// A range of blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub first: u32,
    pub end: u32,
}

impl Region {
    pub fn contains(&self, block: u32) -> bool {
        (self.first..self.end).contains(&block)
    }
    /// Its good blocks, in order.
    pub fn good(self, bad: &BadBlocks) -> impl Iterator<Item = u32> + '_ {
        (self.first..self.end).filter(move |&b| !bad.is_bad(b))
    }
    /// Bytes it holds (good blocks only).
    pub fn capacity(&self, bad: &BadBlocks) -> usize {
        self.good(bad).count() * BLOCK
    }
    /// The block that holds byte `offset` of the data written into this
    /// region (bad blocks skipped), and the offset within it; None past
    /// the region's capacity.
    pub fn locate(&self, bad: &BadBlocks, offset: usize) -> Option<(u32, usize)> {
        let b = self.good(bad).nth(offset / BLOCK)?;
        Some((b, offset % BLOCK))
    }
    /// A sub-range: blocks `first..end` counted from this region's start.
    pub fn sub(&self, first: u32, end: u32) -> Region {
        let r = Region { first: self.first + first, end: self.first + end };
        assert!(r.end <= self.end && r.first <= r.end);
        r
    }
}

/// The bad blocks, one bit per block.
#[derive(Clone, PartialEq, Eq)]
pub struct BadBlocks([u32; BLOCKS as usize / 32]);

impl Default for BadBlocks {
    fn default() -> Self {
        BadBlocks([0; BLOCKS as usize / 32])
    }
}

impl BadBlocks {
    pub fn is_bad(&self, b: u32) -> bool {
        b < BLOCKS && self.0[b as usize / 32] & 1 << (b % 32) != 0
    }
    pub fn mark(&mut self, b: u32) {
        if b < BLOCKS {
            self.0[b as usize / 32] |= 1 << (b % 32);
        }
    }
    pub fn count(&self) -> u32 {
        self.0.iter().map(|w| w.count_ones()).sum()
    }
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        (0..BLOCKS).filter(|&b| self.is_bad(b))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real unit's bad blocks (docs/hardware-toon.md).
    fn toon() -> BadBlocks {
        let mut b = BadBlocks::default();
        for x in [25, 59, 377] {
            b.mark(x);
        }
        b
    }

    #[test]
    fn regions_step_over_bad_blocks_as_uboot_does() {
        let bad = toon();
        assert_eq!(bad.count(), 3);
        // The boot region: block 25 is bad, so byte 128 KiB is in block 26.
        assert_eq!(BOOT.locate(&bad, 0), Some((24, 0)));
        assert_eq!(BOOT.locate(&bad, BLOCK), Some((26, 0)));
        assert_eq!(BOOT.locate(&bad, BLOCK + 5), Some((26, 5)));
        // U-Boot's 3 MiB (24 blocks) from block 24 end in block 48.
        assert_eq!(BOOT.locate(&bad, BOOT_MAX - 1).unwrap().0, 48);
        assert_eq!(BOOT.capacity(&bad), (48 - 2) * BLOCK);
        assert_eq!(SYSTEM.capacity(&bad), (952 - 1) * BLOCK);
        assert_eq!(SYSTEM.locate(&bad, 10_000 * BLOCK), None);
        assert!(!BOOT.contains(FIRST_WRITABLE - 1));
        assert_eq!(SYSTEM.sub(0, 8), Region { first: 72, end: 80 });
        // touchos's regions don't overlap and lie in its area.
        let rs = [STATE, CONFIG, SLOT_A, SLOT_B];
        for w in rs.windows(2) {
            assert_eq!(w[0].end, w[1].first);
        }
        assert!(rs.iter().all(|r| r.first >= SYSTEM.first && r.end <= SYSTEM.end));
        // Block 377 is bad: slot A holds 319 good blocks, B 320.
        assert_eq!(SLOT_A.capacity(&bad), 319 * BLOCK);
        assert_eq!(SLOT_B.capacity(&bad), 320 * BLOCK);
        assert_eq!(slot(b'B'), Some(SLOT_B));
    }
}
