//! The formats touchos keeps on the Toon's NAND, and the uImage header
//! U-Boot wants. Fixed-size encodings (no allocation), so the boot stage
//! can parse them; tested on the host.
//!
//! In touchos's area ([`crate::layout::SYSTEM`]):
//! - **state** records ([`State`], one page each, in a ring of blocks):
//!   which system copy is active; the one with the highest sequence
//!   number wins;
//! - **config** records ([`Config`], one per block, in a ring): touchos.toml,
//!   newest first, the one before it the backup;
//! - two **system copies** ("slots" A and B): a [`SysHeader`] page, then
//!   the kernel (a flat binary for 0xa0008000), the initrd and the boot
//!   info block, each page-aligned. The header is written last, so a
//!   half-written copy has none.
//!
//! All little-endian, every record with a CRC-32 over itself.

/// CRC-32 (IEEE), table-driven, continued from `crc` (start with 0).
pub fn crc32(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in data {
        c = CRC_TABLE[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// One part of a system copy: where it starts (bytes from the copy's
/// start, page-aligned), its length and CRC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Part {
    pub offset: u32,
    pub len: u32,
    pub crc: u32,
}

/// A system copy's first page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SysHeader {
    /// The build's id (NUL-padded text).
    pub build: [u8; 48],
    pub kernel: Part,
    pub initrd: Part,
    pub bootinfo: Part,
}

const SYS_MAGIC: &[u8; 8] = b"tchSYS01";
pub const SYS_HEADER_LEN: usize = 96;

impl SysHeader {
    pub fn encode(&self) -> [u8; SYS_HEADER_LEN] {
        let mut b = [0u8; SYS_HEADER_LEN];
        b[0..8].copy_from_slice(SYS_MAGIC);
        b[8..56].copy_from_slice(&self.build);
        for (i, p) in [self.kernel, self.initrd, self.bootinfo].iter().enumerate() {
            put32(&mut b, 56 + 12 * i, p.offset);
            put32(&mut b, 60 + 12 * i, p.len);
            put32(&mut b, 64 + 12 * i, p.crc);
        }
        let c = crc32(0, &b[..92]);
        put32(&mut b, 92, c);
        b
    }

    pub fn decode(b: &[u8]) -> Option<SysHeader> {
        if b.len() < SYS_HEADER_LEN || &b[0..8] != SYS_MAGIC || crc32(0, &b[..92]) != u32_at(b, 92) {
            return None;
        }
        let part = |i: usize| Part { offset: u32_at(b, 56 + 12 * i), len: u32_at(b, 60 + 12 * i), crc: u32_at(b, 64 + 12 * i) };
        Some(SysHeader { build: b[8..56].try_into().unwrap(), kernel: part(0), initrd: part(1), bootinfo: part(2) })
    }

    /// The build id as text.
    pub fn build_str(&self) -> &str {
        let n = self.build.iter().position(|&c| c == 0).unwrap_or(self.build.len());
        core::str::from_utf8(&self.build[..n]).unwrap_or("?")
    }
}

/// A state record: the active system copy (b'A' or b'B'), a copy on
/// trial (b'A', b'B', or 0: none), and starts without a confirmation
/// (see `crate::system`). Records from before the last two fields have 0
/// there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct State {
    pub seq: u32,
    pub active: u8,
    pub trial: u8,
    pub starts: u8,
}

const STATE_MAGIC: &[u8; 8] = b"tchSTA01";
pub const STATE_LEN: usize = 20;

impl State {
    pub fn encode(&self) -> [u8; STATE_LEN] {
        let mut b = [0u8; STATE_LEN];
        b[0..8].copy_from_slice(STATE_MAGIC);
        put32(&mut b, 8, self.seq);
        b[12] = self.active;
        b[13] = self.trial;
        b[14] = self.starts;
        let c = crc32(0, &b[..16]);
        put32(&mut b, 16, c);
        b
    }
    pub fn decode(b: &[u8]) -> Option<State> {
        if b.len() < STATE_LEN || &b[0..8] != STATE_MAGIC || crc32(0, &b[..16]) != u32_at(b, 16) {
            return None;
        }
        (matches!(b[12], b'A' | b'B') && matches!(b[13], 0 | b'A' | b'B'))
            .then(|| State { seq: u32_at(b, 8), active: b[12], trial: b[13], starts: b[14] })
    }
}

/// What a config record holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
    /// touchos.toml as saved.
    Current = 0,
    /// A file that didn't parse at boot, kept aside (TOUCHOS.BAD).
    Broken = 1,
}

/// A config record's header (the data follows it in the same block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub seq: u32,
    pub kind: ConfigKind,
    pub len: u32,
    pub crc: u32,
}

const CONFIG_MAGIC: &[u8; 8] = b"tchCFG01";
pub const CONFIG_HEADER_LEN: usize = 28;
/// The largest config (the storage protocol's MAX_FILE).
pub const CONFIG_MAX: usize = 64 * 1024;

impl Config {
    pub fn encode(&self) -> [u8; CONFIG_HEADER_LEN] {
        let mut b = [0u8; CONFIG_HEADER_LEN];
        b[0..8].copy_from_slice(CONFIG_MAGIC);
        put32(&mut b, 8, self.seq);
        b[12] = self.kind as u8;
        put32(&mut b, 16, self.len);
        put32(&mut b, 20, self.crc);
        let c = crc32(0, &b[..24]);
        put32(&mut b, 24, c);
        b
    }
    pub fn decode(b: &[u8]) -> Option<Config> {
        if b.len() < CONFIG_HEADER_LEN || &b[0..8] != CONFIG_MAGIC || crc32(0, &b[..24]) != u32_at(b, 24) {
            return None;
        }
        let kind = match b[12] {
            0 => ConfigKind::Current,
            1 => ConfigKind::Broken,
            _ => return None,
        };
        let len = u32_at(b, 16);
        (len as usize <= CONFIG_MAX).then(|| Config { seq: u32_at(b, 8), kind, len, crc: u32_at(b, 20) })
    }
}

/// What the flasher finds in RAM (built by `cargo xtask flash`): the
/// boot image (a uImage), a system copy and a config, each at its offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashPayload {
    pub boot: Part,
    pub system: Part,
    pub config: Part,
    /// The copy to write the system into (b'A' or b'B').
    pub slot: u8,
}

const FLASH_MAGIC: &[u8; 8] = b"tchFLS01";
pub const FLASH_HEADER_LEN: usize = 52;

impl FlashPayload {
    pub fn encode(&self) -> [u8; FLASH_HEADER_LEN] {
        let mut b = [0u8; FLASH_HEADER_LEN];
        b[0..8].copy_from_slice(FLASH_MAGIC);
        for (i, p) in [self.boot, self.system, self.config].iter().enumerate() {
            put32(&mut b, 8 + 12 * i, p.offset);
            put32(&mut b, 12 + 12 * i, p.len);
            put32(&mut b, 16 + 12 * i, p.crc);
        }
        b[44] = self.slot;
        let c = crc32(0, &b[..48]);
        put32(&mut b, 48, c);
        b
    }
    pub fn decode(b: &[u8]) -> Option<FlashPayload> {
        if b.len() < FLASH_HEADER_LEN || &b[0..8] != FLASH_MAGIC || crc32(0, &b[..48]) != u32_at(b, 48) {
            return None;
        }
        let part = |i: usize| Part { offset: u32_at(b, 8 + 12 * i), len: u32_at(b, 12 + 12 * i), crc: u32_at(b, 16 + 12 * i) };
        matches!(b[44], b'A' | b'B').then(|| FlashPayload { boot: part(0), system: part(1), config: part(2), slot: b[44] })
    }
}

/// A legacy uImage header (big-endian, 64 bytes) for an uncompressed ARM
/// "Linux kernel" image loaded at and started from `load`: what the
/// Toon's U-Boot `bootm` takes. Before jumping it turns the caches off.
pub fn uimage_header(payload: &[u8], load: u32, name: &str, time: u32) -> [u8; 64] {
    let mut h = [0u8; 64];
    let be = |h: &mut [u8; 64], at: usize, v: u32| h[at..at + 4].copy_from_slice(&v.to_be_bytes());
    be(&mut h, 0, 0x2705_1956);
    be(&mut h, 8, time);
    be(&mut h, 12, payload.len() as u32);
    be(&mut h, 16, load);
    be(&mut h, 20, load);
    be(&mut h, 24, crc32(0, payload));
    h[28] = 5; // OS: Linux (bootm's Linux path: r1 = machine, caches off)
    h[29] = 2; // ARM
    h[30] = 2; // a kernel
    h[31] = 0; // uncompressed
    let n = name.len().min(31);
    h[32..32 + n].copy_from_slice(&name.as_bytes()[..n]);
    let hc = crc32(0, &h);
    be(&mut h, 4, hc);
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32(0, b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(crc32(0, b"1234"), b"56789"), 0xcbf4_3926);
    }

    #[test]
    fn records_round_trip_and_reject_damage() {
        let mut build = [0u8; 48];
        build[..5].copy_from_slice(b"abc12");
        let h = SysHeader {
            build,
            kernel: Part { offset: 2048, len: 500_000, crc: 1 },
            initrd: Part { offset: 503_808, len: 8_000_000, crc: 2 },
            bootinfo: Part { offset: 8_503_296, len: 536, crc: 3 },
        };
        let mut b = h.encode();
        assert_eq!(SysHeader::decode(&b), Some(h));
        assert_eq!(SysHeader::decode(&b).unwrap().build_str(), "abc12");
        b[60] ^= 1;
        assert_eq!(SysHeader::decode(&b), None);
        // An erased page is no record.
        assert_eq!(SysHeader::decode(&[0xff; 2048]), None);
        assert_eq!(State::decode(&[0xff; 64]), None);
        assert_eq!(Config::decode(&[0xff; 64]), None);

        let s = State { seq: 7, active: b'B', trial: b'A', starts: 2 };
        assert_eq!(State::decode(&s.encode()), Some(s));
        // A record from before trial/starts: zeros there.
        let mut old = s.encode();
        old[13] = 0;
        old[14] = 0;
        let c = crc32(0, &old[..16]);
        old[16..20].copy_from_slice(&c.to_le_bytes());
        assert_eq!(State::decode(&old), Some(State { trial: 0, starts: 0, ..s }));
        let c = Config { seq: 3, kind: ConfigKind::Broken, len: 1234, crc: 99 };
        assert_eq!(Config::decode(&c.encode()), Some(c));
        let big = Config { len: CONFIG_MAX as u32 + 1, ..c };
        assert_eq!(Config::decode(&big.encode()), None);
        let f = FlashPayload { boot: Part { offset: 2048, len: 10, crc: 4 }, system: Part::default(), config: Part::default(), slot: b'A' };
        assert_eq!(FlashPayload::decode(&f.encode()), Some(f));
    }

    #[test]
    fn uimage_header_as_mkimage_makes_it() {
        let h = uimage_header(b"hello", 0xa700_0000, "touchos nandboot", 0);
        assert_eq!(&h[0..4], &[0x27, 0x05, 0x19, 0x56]);
        assert_eq!(u32::from_be_bytes(h[12..16].try_into().unwrap()), 5);
        assert_eq!(u32::from_be_bytes(h[16..20].try_into().unwrap()), 0xa700_0000);
        assert_eq!(u32::from_be_bytes(h[24..28].try_into().unwrap()), crc32(0, b"hello"));
        // The header CRC is over the header with its own field zero.
        let mut z = h;
        z[4..8].fill(0);
        assert_eq!(u32::from_be_bytes(h[4..8].try_into().unwrap()), crc32(0, &z));
        assert_eq!(&h[28..32], &[5, 2, 2, 0]);
    }
}
