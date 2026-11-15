//! The i.MX27's GPIO module: ports A-F, 0x100 apart, each with the same
//! registers. A pin is a GPIO only while its GIUS bit is set; otherwise it
//! is on its primary function, or (GPR set) its alternate one. (The
//! i.MX25's GPIO is a different design; only the Toon uses this.)

use super::rmw;

const BASE: u64 = 0x1001_5000;

pub const DDIR: usize = 0x00;
/// Output configuration, 2 bits per pin: OCR1 for pins 0-15, OCR2 16-31.
pub const OCR1: usize = 0x04;
pub const OCR2: usize = 0x08;
/// Input configuration A and B, 2 bits per pin (pins 0-15, 16-31).
pub const ICONFA1: usize = 0x0c;
pub const ICONFA2: usize = 0x10;
pub const ICONFB1: usize = 0x14;
pub const ICONFB2: usize = 0x18;
pub const DR: usize = 0x1c;
pub const GIUS: usize = 0x20;
/// Sample status: the pins' levels.
pub const SSR: usize = 0x24;
pub const GPR: usize = 0x38;
pub const PUEN: usize = 0x40;

/// OCR value: the pin's output comes from DR (0 would be the peripheral's
/// A input, AIN).
pub const OCR_DR: u32 = 0b11;

/// One port's registers (mapped).
#[derive(Clone, Copy)]
pub struct Port(usize);

/// Port `'A'` to `'F'`.
pub fn port(letter: char) -> Port {
    assert!(('A'..='F').contains(&letter), "no GPIO port {letter}");
    let n = letter as u64 - 'A' as u64;
    Port(crate::mem::map_mmio(BASE + n * 0x100, 0x100))
}

impl Port {
    pub fn read(self, off: usize) -> u32 {
        unsafe { ((self.0 + off) as *const u32).read_volatile() }
    }

    /// Clear the bits in `clear`, then set those in `set`.
    pub fn rmw(self, off: usize, clear: u32, set: u32) {
        rmw(self.0 + off, clear, set);
    }

    /// The OCR field of every pin in `pins` to `val` (`OCR_DR`...).
    pub fn set_ocr(self, pins: u32, val: u32) {
        for p in (0..32).filter(|p| pins & 1 << p != 0) {
            let shift = p % 16 * 2;
            self.rmw(if p < 16 { OCR1 } else { OCR2 }, 0b11 << shift, val << shift);
        }
    }
}
