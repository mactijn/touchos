//! Device registers: [`Regs`], a mapped register window, instead of
//! hand-written volatile helpers in every driver.
//!
//! Every access is volatile and exactly as wide as the method says (a
//! 16-bit register is read with one 16-bit load, never as part of a word).
//! Each access checks, in every build, that it lies inside the window and
//! is aligned to its width, so safe code cannot reach outside the mapping:
//! a compare next to an uncached device access costs next to nothing.

/// A window of device registers: `len` bytes at `base`, mapped for the
/// life of the process (drivers never unmap their registers).
///
/// A plain address and length, so it is `Copy` and `Send`: a driver keeps
/// one per register block and hands out sub-windows ([`Regs::at`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Regs {
    base: usize,
    len: usize,
}

/// Look until `look` returns something, for `timeout_us` at most by the
/// clock `now_us` (microseconds), spinning between looks. None if it never
/// did.
///
/// The clock is read before each look: if this process was preempted past
/// the deadline, there is still one more look, and a condition that came
/// true meanwhile is not reported as a timeout. Every polling wait in the
/// drivers goes through here (commit 346a937: a preemption longer than a
/// short deadline made a finished step look like a failure).
pub fn poll_until<T>(
    mut now_us: impl FnMut() -> u64,
    timeout_us: u64,
    mut look: impl FnMut() -> Option<T>,
) -> Option<T> {
    let end = now_us().saturating_add(timeout_us);
    loop {
        let late = now_us() > end;
        if let Some(v) = look() {
            return Some(v);
        }
        if late {
            return None;
        }
        core::hint::spin_loop();
    }
}

/// [`Regs::wait32`] gave up: the register never matched. Holds the last
/// value read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeout(pub u32);

impl Regs {
    /// Map `len` bytes at `off` in the MMIO capability `cap` (`mmio_map`).
    #[cfg(target_os = "none")]
    pub fn from_cap(cap: u32, off: u64, len: usize) -> touchos_rt::sys::Result<Regs> {
        let va = touchos_rt::sys::mmio_map(cap, off, len as u64)?;
        Ok(Regs {
            base: va as usize,
            len,
        })
    }

    /// Registers that are already mapped at `base` (tests use plain
    /// memory; drivers use [`from_cap`](Self::from_cap)).
    ///
    /// # Safety
    /// `base..base + len` must stay mapped (device memory, or for tests
    /// plain memory) for as long as the window or a copy of it is used.
    pub const unsafe fn new(base: usize, len: usize) -> Regs {
        Regs { base, len }
    }

    /// The sub-window from `off` to the end of this one (e.g. EHCI's
    /// operational registers at CAPLENGTH). Panics if `off` is past the end.
    pub fn at(self, off: usize) -> Regs {
        assert!(off <= self.len, "mmio: window at {off:#x} past the end ({:#x})", self.len);
        Regs {
            base: self.base + off,
            len: self.len - off,
        }
    }

    /// The window's virtual address.
    pub fn base(&self) -> usize {
        self.base
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    fn addr(&self, off: usize, width: usize) -> usize {
        assert!(
            off.checked_add(width).is_some_and(|end| end <= self.len),
            "mmio: {width}-byte access at {off:#x} outside the window ({:#x})",
            self.len
        );
        assert!(
            (self.base + off) % width == 0,
            "mmio: unaligned {width}-byte access at {off:#x}"
        );
        self.base + off
    }

    #[inline]
    pub fn r8(&self, off: usize) -> u8 {
        unsafe { (self.addr(off, 1) as *const u8).read_volatile() }
    }
    #[inline]
    pub fn r16(&self, off: usize) -> u16 {
        unsafe { (self.addr(off, 2) as *const u16).read_volatile() }
    }
    #[inline]
    pub fn r32(&self, off: usize) -> u32 {
        unsafe { (self.addr(off, 4) as *const u32).read_volatile() }
    }
    #[inline]
    pub fn w8(&self, off: usize, v: u8) {
        unsafe { (self.addr(off, 1) as *mut u8).write_volatile(v) }
    }
    #[inline]
    pub fn w16(&self, off: usize, v: u16) {
        unsafe { (self.addr(off, 2) as *mut u16).write_volatile(v) }
    }
    #[inline]
    pub fn w32(&self, off: usize, v: u32) {
        unsafe { (self.addr(off, 4) as *mut u32).write_volatile(v) }
    }

    /// Read, clear the bits `clear`, set the bits `set`, write back (one
    /// 32-bit read, then one 32-bit write). Returns the value written.
    #[inline]
    pub fn rmw32(&self, off: usize, clear: u32, set: u32) -> u32 {
        let v = (self.r32(off) & !clear) | set;
        self.w32(off, v);
        v
    }

    /// Poll until `r32(off) & mask == value`, for `timeout_us` at most
    /// ([`poll_until`]: the register gets a last look after the deadline).
    /// Returns the matching value, or the last one read.
    #[cfg(target_os = "none")]
    pub fn wait32(&self, off: usize, mask: u32, value: u32, timeout_us: u64) -> Result<u32, Timeout> {
        self.wait32_by(touchos_rt::time::now_us, off, mask, value, timeout_us)
    }

    /// [`wait32`](Self::wait32) with the clock `now_us`.
    #[cfg(any(target_os = "none", test))]
    fn wait32_by(
        &self,
        now_us: impl FnMut() -> u64,
        off: usize,
        mask: u32,
        value: u32,
        timeout_us: u64,
    ) -> Result<u32, Timeout> {
        let mut last = 0;
        poll_until(now_us, timeout_us, || {
            last = self.r32(off);
            (last & mask == value).then_some(last)
        })
        .ok_or(Timeout(last))
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use core::cell::Cell;
    use std::vec;

    /// A register window over plain (u32-aligned) memory.
    fn window(words: &mut [u32]) -> Regs {
        unsafe { Regs::new(words.as_mut_ptr() as usize, words.len() * 4) }
    }

    #[test]
    fn widths_and_offsets() {
        let mut mem = vec![0u32; 4];
        let r = window(&mut mem);
        r.w32(0, 0x1122_3344);
        r.w16(4, 0xabcd);
        r.w16(6, 0x1234);
        r.w8(8, 0x5a);
        r.w8(11, 0xa5);
        assert_eq!(r.r32(0), 0x1122_3344);
        assert_eq!(r.r16(4), 0xabcd);
        assert_eq!(r.r16(6), 0x1234);
        assert_eq!(r.r8(8), 0x5a);
        assert_eq!(r.r8(11), 0xa5);
        // Narrow writes touch only their own bytes.
        let w1 = mem[1].to_ne_bytes();
        assert_eq!([w1[0], w1[1]], 0xabcdu16.to_ne_bytes());
        assert_eq!([w1[2], w1[3]], 0x1234u16.to_ne_bytes());
        assert_eq!(mem[2].to_ne_bytes(), [0x5a, 0, 0, 0xa5]);
        assert_eq!(mem[3], 0);
    }

    #[test]
    fn sub_windows() {
        let mut mem = vec![0u32; 8];
        let r = window(&mut mem);
        let op = r.at(0x10);
        assert_eq!(op.len(), 0x10);
        assert_eq!(op.base(), r.base() + 0x10);
        op.w32(4, 7);
        assert_eq!(r.r32(0x14), 7);
        assert!(r.at(0x20).is_empty());
    }

    #[test]
    fn rmw() {
        let mut mem = vec![0xf0f0_f0f0u32; 1];
        let r = window(&mut mem);
        assert_eq!(r.rmw32(0, 0x0000_00f0, 0x0000_0001), 0xf0f0_f001);
        assert_eq!(r.rmw32(0, 0, 1 << 8), 0xf0f0_f101);
        assert_eq!(r.rmw32(0, 0xffff_0000, 0), 0x0000_f101);
        assert_eq!(mem[0], 0x0000_f101);
    }

    #[test]
    fn wait_matches() {
        let mut mem = vec![0x8000_0001u32; 1];
        let r = window(&mut mem);
        assert_eq!(r.wait32_by(|| 0, 0, 0x8000_0000, 0x8000_0000, 10), Ok(0x8000_0001));
        assert_eq!(r.wait32_by(|| 0, 0, 0x2, 0, 10), Ok(0x8000_0001));
    }

    #[test]
    fn wait_times_out_after_a_last_look() {
        let mut mem = vec![0u32; 1];
        let r = window(&mut mem);
        // A clock that advances 10 µs per reading: with a 25 µs timeout
        // the deadline is 25; the clock reads 10, 20, 30: the third look
        // is late but still reads the register.
        let t = Cell::new(0u64);
        let clock = || {
            let v = t.get();
            t.set(v + 10);
            v
        };
        assert_eq!(r.wait32_by(clock, 0, 1, 1, 25), Err(Timeout(0)));
        assert_eq!(t.get(), 40); // start + three looks
    }

    #[test]
    fn wait_late_look_still_succeeds() {
        // Preempted past the deadline before the first look: the register
        // matched meanwhile, so it is not a timeout.
        let mut mem = vec![1u32; 1];
        let r = window(&mut mem);
        let t = Cell::new(0u64);
        let clock = || {
            let v = t.get();
            t.set(v + 1_000);
            v
        };
        assert_eq!(r.wait32_by(clock, 0, 1, 1, 10), Ok(1));
    }

    #[test]
    fn poll_until_looks_once_more_after_the_deadline() {
        // The clock jumps past the deadline between the start and the first
        // look (a preemption): one look, which succeeds.
        let t = Cell::new(0u64);
        let clock = || {
            let v = t.get();
            t.set(v + 1_000);
            v
        };
        let looks = Cell::new(0);
        assert_eq!(poll_until(clock, 10, || { looks.set(looks.get() + 1); Some(7) }), Some(7));
        assert_eq!(looks.get(), 1);
        // Never true: looks until the clock is past the deadline, then one
        // more, then gives up. Deadline 25; looks at 10, 20, 30.
        t.set(0);
        let step = || {
            let v = t.get();
            t.set(v + 10);
            v
        };
        looks.set(0);
        assert_eq!(poll_until(step, 25, || { looks.set(looks.get() + 1); None::<()> }), None);
        assert_eq!(looks.get(), 3);
        // A deadline beyond the clock's range does not overflow.
        assert_eq!(poll_until(|| u64::MAX - 1, u64::MAX, || Some(())), Some(()));
    }

    #[test]
    #[should_panic]
    fn out_of_range_is_caught() {
        let mut mem = vec![0u32; 1];
        window(&mut mem).r32(4);
    }

    #[test]
    #[should_panic]
    fn window_past_the_end_is_caught() {
        let mut mem = vec![0u32; 1];
        window(&mut mem).at(8);
    }

    #[test]
    #[should_panic]
    fn unaligned_is_caught() {
        let mut mem = vec![0u32; 2];
        window(&mut mem).r32(2);
    }
}
