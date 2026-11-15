//! Every atomic operation, built from a 32-bit and a 64-bit compare-and-swap.
//!
//! 8- and 16-bit operations work on the aligned 32-bit word that contains
//! them (little endian), the way compiler-builtins does it for ARM Linux.

use core::ptr::read_volatile;

/// The primitives a backend provides. All must be atomic with respect to
/// every other thread on this (single) CPU.
pub trait Prim {
    /// Stores `new` if `*p == old`; returns the value found.
    unsafe fn cas32(p: *mut u32, old: u32, new: u32) -> u32;
    /// Stores `new` if `*p == old`; returns the value found.
    unsafe fn cas64(p: *mut u64, old: u64, new: u64) -> u64;
    unsafe fn load64(p: *const u64) -> u64;
    unsafe fn store64(p: *mut u64, v: u64);
}

/// Applies `f` to `*p` atomically; returns the old value.
pub unsafe fn rmw32<P: Prim>(p: *mut u32, f: impl Fn(u32) -> u32) -> u32 {
    let mut old = unsafe { read_volatile(p) };
    loop {
        let prev = unsafe { P::cas32(p, old, f(old)) };
        if prev == old {
            return old;
        }
        old = prev;
    }
}

/// Applies `f` to `*p` atomically; returns the old value.
pub unsafe fn rmw64<P: Prim>(p: *mut u64, f: impl Fn(u64) -> u64) -> u64 {
    let mut old = unsafe { P::load64(p) };
    loop {
        let prev = unsafe { P::cas64(p, old, f(old)) };
        if prev == old {
            return old;
        }
        old = prev;
    }
}

/// Where an 8/16-bit value sits in its aligned word.
#[derive(Clone, Copy)]
struct Lane {
    word: *mut u32,
    shift: u32,
    mask: u32, // already shifted
}

fn lane(p: *mut u8, size: u32) -> Lane {
    let addr = p as usize;
    let shift = ((addr & 3) * 8) as u32;
    let bits: u32 = if size == 1 { 0xff } else { 0xffff };
    Lane { word: (addr & !3) as *mut u32, shift, mask: bits << shift }
}

impl Lane {
    fn get(self, w: u32) -> u32 {
        (w & self.mask) >> self.shift
    }
    fn put(self, w: u32, v: u32) -> u32 {
        (w & !self.mask) | ((v << self.shift) & self.mask)
    }
}

/// Applies `f` to the 8/16-bit value at `p` (zero-extended); returns the old
/// value (zero-extended).
pub unsafe fn rmw_small<P: Prim>(p: *mut u8, size: u32, f: impl Fn(u32) -> u32) -> u32 {
    let l = lane(p, size);
    let old = unsafe { rmw32::<P>(l.word, |w| l.put(w, f(l.get(w)))) };
    l.get(old)
}

/// Compare-and-swap of the 8/16-bit value at `p`; returns the value found.
pub unsafe fn cas_small<P: Prim>(p: *mut u8, size: u32, old: u32, new: u32) -> u32 {
    let l = lane(p, size);
    let (old, new) = (old & (l.mask >> l.shift), new & (l.mask >> l.shift));
    let mut w = unsafe { read_volatile(l.word) };
    loop {
        let cur = l.get(w);
        if cur != old {
            return cur;
        }
        // The other bytes of the word may change under us: retry on them.
        let prev = unsafe { P::cas32(l.word, w, l.put(w, new)) };
        if prev == w {
            return old;
        }
        w = prev;
    }
}

/// Sign-extends an `size`-byte value.
pub fn sext(v: u32, size: u32) -> i32 {
    match size {
        1 => v as u8 as i8 as i32,
        2 => v as u16 as i16 as i32,
        _ => v as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    // Single-threaded stand-in; counts calls so retries are visible.
    thread_local!(static CALLS: Cell<u32> = const { Cell::new(0) });
    struct Host;
    impl Prim for Host {
        unsafe fn cas32(p: *mut u32, old: u32, new: u32) -> u32 {
            CALLS.with(|c| c.set(c.get() + 1));
            let cur = unsafe { *p };
            if cur == old {
                unsafe { *p = new };
            }
            cur
        }
        unsafe fn cas64(p: *mut u64, old: u64, new: u64) -> u64 {
            let cur = unsafe { *p };
            if cur == old {
                unsafe { *p = new };
            }
            cur
        }
        unsafe fn load64(p: *const u64) -> u64 {
            unsafe { *p }
        }
        unsafe fn store64(p: *mut u64, v: u64) {
            unsafe { *p = v }
        }
    }

    #[test]
    fn word_ops() {
        let mut x: u32 = 10;
        let old = unsafe { rmw32::<Host>(&mut x, |v| v.wrapping_add(5)) };
        assert_eq!((old, x), (10, 15));
        let old = unsafe { rmw32::<Host>(&mut x, |v| !(v & 0xf0)) };
        assert_eq!((old, x), (15, !0));
    }

    #[test]
    fn bytes_stay_in_their_lane() {
        #[repr(align(4))]
        struct W([u8; 4]);
        let mut w = W([0x11, 0xfe, 0x33, 0x44]);
        let p = &mut w.0[1] as *mut u8;
        let old = unsafe { rmw_small::<Host>(p, 1, |v| v.wrapping_add(3)) };
        assert_eq!(old, 0xfe);
        assert_eq!(w.0, [0x11, 0x01, 0x33, 0x44]); // wrapped inside the byte
        let found = unsafe { cas_small::<Host>(&mut w.0[3], 1, 0x44, 0x99) };
        assert_eq!(found, 0x44);
        assert_eq!(w.0, [0x11, 0x01, 0x33, 0x99]);
        let found = unsafe { cas_small::<Host>(&mut w.0[0], 1, 0x00, 0x77) };
        assert_eq!(found, 0x11); // mismatch: unchanged
        assert_eq!(w.0, [0x11, 0x01, 0x33, 0x99]);
    }

    #[test]
    fn halfwords() {
        #[repr(align(4))]
        struct W([u16; 2]);
        let mut w = W([0x1234, 0xffff]);
        let old = unsafe { rmw_small::<Host>(&mut w.0[1] as *mut u16 as *mut u8, 2, |v| v + 1) };
        assert_eq!(old, 0xffff);
        assert_eq!(w.0, [0x1234, 0x0000]);
    }

    #[test]
    fn signed_max_on_bytes() {
        #[repr(align(4))]
        struct W([u8; 4]);
        let mut w = W([0, 0, 0xf0, 0]); // -16
        let p = &mut w.0[2] as *mut u8;
        unsafe { rmw_small::<Host>(p, 1, |v| sext(v, 1).max(-3) as u32) };
        assert_eq!(w.0[2] as i8, -3);
        unsafe { rmw_small::<Host>(p, 1, |v| (v).max(0x10)) }; // unsigned: 0xfd stays
        assert_eq!(w.0[2], 0xfd);
    }

    #[test]
    fn sixty_four() {
        let mut x: u64 = u32::MAX as u64;
        let old = unsafe { rmw64::<Host>(&mut x, |v| v + 1) };
        assert_eq!((old, x), (u32::MAX as u64, 1 << 32));
        assert_eq!(unsafe { Host::cas64(&mut x, 5, 6) }, 1 << 32);
    }
}
