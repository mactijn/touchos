//! How an i.MX board (the Eneco Toon v1, QEMU's imx25-pdk) is booted: the
//! contract between the loader (xtask/src/arm.rs: QEMU's `-kernel` and
//! `-device loader`, or OpenOCD's `load_image` on the Toon) and the kernel
//! (kernel/src/board/imx/boot.rs; its build script writes the board's
//! numbers into the linker script).
//!
//! Everything is loaded into RAM at fixed offsets from the board's RAM
//! base: the kernel ELF by its load addresses (its entry, `_start`, at
//! [`KERNEL`]), the boot-info block ([`BootBlock`]) at [`BOOTINFO`], the
//! config at [`CONFIG`] and the initrd at [`INITRD`].
//!
//! Not the per-process [`crate::BootInfo`]: this block is read once, by
//! the kernel.

/// What the loader and the kernel need to know about a board.
#[derive(Clone, Copy, Debug)]
pub struct Board {
    /// Its name: `--board` for xtask, the kernel's `board-<name>` feature,
    /// and the `board=` the kernel gives init.
    pub name: &'static str,
    /// The physical address of RAM ([`RAM_SIZE`] bytes from here).
    pub ram_base: u64,
    /// UART1, the console (the start-up code maps it for the early
    /// console).
    pub uart: u64,
}

/// The Eneco Toon v1 (i.MX27).
pub const TOON: Board = Board {
    name: "toon",
    ram_base: 0xa000_0000,
    uart: 0x1000_a000,
};

/// QEMU's imx25-pdk (i.MX25), the test bench.
pub const IMX25: Board = Board {
    name: "imx25",
    ram_base: 0x8000_0000,
    uart: 0x43f9_0000,
};

/// RAM on both boards (QEMU gets `-m 128M`).
pub const RAM_SIZE: u64 = 128 << 20;

/// Where the kernel sees RAM: the start-up code maps RAM here, and the
/// kernel is linked here (physical = virtual - `KERNEL_VIRT` + RAM base).
pub const KERNEL_VIRT: u64 = 0xc000_0000;

// The load layout: offsets from the RAM base.

/// The kernel: its entry (`_start`) is the first byte it loads.
pub const KERNEL: u64 = 0x8000;
/// The boot-info block ([`BootBlock`], [`BLOCK_LEN`] bytes).
pub const BOOTINFO: u64 = 0x0100_0000;
/// The config file (touchos.toml), up to [`CONFIG_MAX`] bytes.
pub const CONFIG: u64 = 0x0101_0000;
/// The framebuffer (800x480 RGB565): not RAM for the kernel. On the Toon
/// U-Boot set up the LCD controller to scan it out; QEMU's i.MX25 has no
/// LCD controller, so there it is plain RAM the dashboard draws into
/// (read it back with the monitor's `pmemsave`).
pub const FRAMEBUFFER: u64 = 0x0140_0000;
/// Its geometry: RGB565, 2 bytes per pixel, rows without padding.
pub const FB_WIDTH: u32 = 800;
pub const FB_HEIGHT: u32 = 480;
pub const FB_BPP: u16 = 16;
pub const FRAMEBUFFER_LEN: u64 = FB_WIDTH as u64 * FB_HEIGHT as u64 * (FB_BPP as u64 / 8);
/// The config must end before the framebuffer.
pub const CONFIG_MAX: u64 = FRAMEBUFFER - CONFIG;
/// The initrd (ustar), up to the end of RAM.
pub const INITRD: u64 = 0x0200_0000;
pub const INITRD_MAX: u64 = RAM_SIZE - INITRD;

// In order, and nothing overlaps.
const _: () = assert!(BOOTINFO + BLOCK_LEN as u64 <= CONFIG);
const _: () = assert!(CONFIG < FRAMEBUFFER && FRAMEBUFFER + FRAMEBUFFER_LEN <= INITRD);
const _: () = assert!(INITRD < RAM_SIZE);
// The start-up code maps RAM in 1 MiB sections.
const _: () = assert!(RAM_SIZE.is_multiple_of(1 << 20));
const _: () = assert!(TOON.ram_base.is_multiple_of(1 << 20) && IMX25.ram_base.is_multiple_of(1 << 20));

/// The boot-info block's first bytes.
pub const MAGIC: &[u8; 8] = b"tchBOOT1";
/// The longest command line.
pub const CMDLINE_MAX: usize = 512;
/// The magic and four 32-bit words.
const HEADER_LEN: usize = 24;
/// The block's size: the header, then the command line's room.
pub const BLOCK_LEN: usize = HEADER_LEN + CMDLINE_MAX;

/// The boot-info block at [`BOOTINFO`], little endian:
///
/// | offset | size | |
/// |---|---|---|
/// | 0  | 8 | [`MAGIC`] |
/// | 8  | 4 | the initrd's length |
/// | 12 | 4 | the config's length |
/// | 16 | 4 | the command line's length |
/// | 20 | 4 | reserved (0) |
/// | 24 | [`CMDLINE_MAX`] | the command line, zero-padded |
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BootBlock<'a> {
    pub initrd_len: u32,
    pub config_len: u32,
    pub cmdline: &'a [u8],
}

impl<'a> BootBlock<'a> {
    /// The block's bytes, or `None` if something doesn't fit the layout
    /// (a command line over [`CMDLINE_MAX`] bytes, a config over
    /// [`CONFIG_MAX`], an initrd over [`INITRD_MAX`]).
    pub fn encode(&self) -> Option<[u8; BLOCK_LEN]> {
        if !self.fits() {
            return None;
        }
        let mut b = [0; BLOCK_LEN];
        b[..8].copy_from_slice(MAGIC);
        b[8..12].copy_from_slice(&self.initrd_len.to_le_bytes());
        b[12..16].copy_from_slice(&self.config_len.to_le_bytes());
        b[16..20].copy_from_slice(&(self.cmdline.len() as u32).to_le_bytes());
        b[HEADER_LEN..HEADER_LEN + self.cmdline.len()].copy_from_slice(self.cmdline);
        Some(b)
    }

    /// Read a block: `None` if `b` is no boot-info block (too short, the
    /// wrong magic) or its lengths don't fit the layout.
    pub fn decode(b: &'a [u8]) -> Option<Self> {
        if b.len() < BLOCK_LEN || &b[..8] != MAGIC {
            return None;
        }
        let word = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let cmdline_len = word(16) as usize;
        if cmdline_len > CMDLINE_MAX {
            return None;
        }
        let block = BootBlock {
            initrd_len: word(8),
            config_len: word(12),
            cmdline: &b[HEADER_LEN..HEADER_LEN + cmdline_len],
        };
        block.fits().then_some(block)
    }

    fn fits(&self) -> bool {
        self.cmdline.len() <= CMDLINE_MAX
            && self.config_len as u64 <= CONFIG_MAX
            && self.initrd_len as u64 <= INITRD_MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: BootBlock = BootBlock {
        initrd_len: 0x0076_da00,
        config_len: 430,
        cmdline: b"loglevel=debug foo=bar",
    };

    #[test]
    fn round_trip() {
        let b = SAMPLE.encode().unwrap();
        assert_eq!(BootBlock::decode(&b), Some(SAMPLE));
        let empty = BootBlock { initrd_len: 0, config_len: 0, cmdline: b"" };
        assert_eq!(BootBlock::decode(&empty.encode().unwrap()), Some(empty));
        let long = [b'x'; CMDLINE_MAX];
        let full = BootBlock { initrd_len: INITRD_MAX as u32, config_len: CONFIG_MAX as u32, cmdline: &long };
        assert_eq!(BootBlock::decode(&full.encode().unwrap()), Some(full));
    }

    /// The bytes, field by field (what the kernel has always read).
    #[test]
    fn layout() {
        let b = SAMPLE.encode().unwrap();
        assert_eq!(b.len(), 536);
        assert_eq!(&b[..8], b"tchBOOT1");
        assert_eq!(b[8..12], [0x00, 0xda, 0x76, 0x00]);
        assert_eq!(b[12..16], [0xae, 0x01, 0, 0]);
        assert_eq!(b[16..20], [22, 0, 0, 0]);
        assert_eq!(b[20..24], [0; 4]);
        assert_eq!(&b[24..46], b"loglevel=debug foo=bar");
        assert!(b[46..].iter().all(|&x| x == 0));
    }

    #[test]
    fn encode_rejects_what_does_not_fit() {
        let long = [b'x'; CMDLINE_MAX + 1];
        assert_eq!(BootBlock { cmdline: &long, ..SAMPLE }.encode(), None);
        assert_eq!(BootBlock { config_len: CONFIG_MAX as u32 + 1, ..SAMPLE }.encode(), None);
        assert_eq!(BootBlock { initrd_len: INITRD_MAX as u32 + 1, ..SAMPLE }.encode(), None);
    }

    #[test]
    fn decode_rejects_damage() {
        let good = SAMPLE.encode().unwrap();
        let damaged = |at: usize, v: u8| {
            let mut b = good;
            b[at] = v;
            b
        };
        // The magic.
        assert_eq!(BootBlock::decode(&damaged(0, b'T')), None);
        assert_eq!(BootBlock::decode(&damaged(7, b'2')), None);
        assert_eq!(BootBlock::decode(&[0; BLOCK_LEN]), None);
        // Too short.
        assert_eq!(BootBlock::decode(&good[..BLOCK_LEN - 1]), None);
        assert_eq!(BootBlock::decode(&good[..8]), None);
        assert_eq!(BootBlock::decode(&[]), None);
        // A command line longer than its room (513).
        let mut b = good;
        b[16..20].copy_from_slice(&(CMDLINE_MAX as u32 + 1).to_le_bytes());
        assert_eq!(BootBlock::decode(&b), None);
        // Lengths past the layout: the initrd past the end of RAM, the
        // config into the framebuffer.
        assert_eq!(BootBlock::decode(&damaged(11, 0x10)), None);
        assert_eq!(BootBlock::decode(&damaged(14, 0x40)), None);
        // A longer slice is fine: only the block is read.
        let mut more = [0xffu8; BLOCK_LEN + 16];
        more[..BLOCK_LEN].copy_from_slice(&good);
        assert_eq!(BootBlock::decode(&more), Some(SAMPLE));
    }
}
