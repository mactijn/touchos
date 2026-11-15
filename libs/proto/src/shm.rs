//! Structures in shared memory (a memory object mapped by two processes).
//!
//! - [`Ring`]: single-producer single-consumer message queue.
//! - [`Board`]: one published value (a snapshot) that readers copy out; a
//!   sequence number tells them when it changed (a seqlock).
//!
//! Neither side ever waits for the other inside these structures; waking
//! the consumer (a notification) is up to the caller of `push`.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

// ---- ring ----

#[repr(C)]
struct RingHeader {
    /// Next slot to write (producer).
    head: AtomicU32,
    /// Next slot to read (consumer).
    tail: AtomicU32,
    slot_size: AtomicU32,
    /// 0 until the producer set the ring up.
    slots: AtomicU32,
    /// The consumer found the ring empty and is waiting for a signal.
    consumer_waiting: AtomicU32,
    /// Bumped by a producer that (re)started.
    generation: AtomicU32,
}

const RING_HEADER: usize = 64;

/// Bytes needed for a ring of `slots` slots of `slot_size` bytes.
pub const fn ring_size(slots: u32, slot_size: u32) -> usize {
    RING_HEADER + slots as usize * stride(slot_size)
}

const fn stride(slot_size: u32) -> usize {
    (4 + slot_size as usize).div_ceil(8) * 8
}

/// A view of a ring in memory. Layout: a 64-byte header, then slots of
/// `4 + slot_size` bytes (rounded up to 8): a length, then the bytes.
pub struct Ring {
    base: *mut u8,
}

unsafe impl Send for Ring {}

/// Result of [`Ring::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    /// Queued; the consumer may be asleep: signal it.
    Wake,
    /// Queued; the consumer is busy anyway.
    Queued,
    /// The ring is full (or the message too long): dropped.
    Full,
}

impl Ring {
    /// # Safety
    /// `mem` must point to at least `ring_size(..)` bytes of memory shared
    /// only as this ring, valid as long as the `Ring` is used.
    pub unsafe fn at(mem: *mut u8) -> Ring {
        Ring { base: mem }
    }

    fn hdr(&self) -> &RingHeader {
        unsafe { &*(self.base as *const RingHeader) }
    }

    fn slot(&self, i: u32) -> *mut u8 {
        let h = self.hdr();
        let slots = h.slots.load(Ordering::Relaxed).max(1);
        let stride = stride(h.slot_size.load(Ordering::Relaxed));
        unsafe { self.base.add(RING_HEADER + (i % slots) as usize * stride) }
    }

    /// Producer: (re)initialize. The consumer sees an empty ring.
    pub fn init(&self, slots: u32, slot_size: u32) {
        let h = self.hdr();
        h.slot_size.store(slot_size, Ordering::Relaxed);
        h.head
            .store(h.tail.load(Ordering::Acquire), Ordering::Relaxed);
        h.generation.fetch_add(1, Ordering::Relaxed);
        h.slots.store(slots, Ordering::Release);
    }

    /// Nothing queued?
    pub fn is_empty(&self) -> bool {
        let h = self.hdr();
        h.head.load(Ordering::Acquire) == h.tail.load(Ordering::Acquire)
    }

    /// Producer: is there room for another message?
    pub fn has_room(&self) -> bool {
        let h = self.hdr();
        let slots = h.slots.load(Ordering::Acquire);
        slots != 0
            && h.head
                .load(Ordering::Relaxed)
                .wrapping_sub(h.tail.load(Ordering::Acquire))
                < slots
    }

    /// Producer: queue one message.
    pub fn push(&self, data: &[u8]) -> Pushed {
        let h = self.hdr();
        let slots = h.slots.load(Ordering::Acquire);
        if slots == 0 || data.len() > h.slot_size.load(Ordering::Relaxed) as usize {
            return Pushed::Full;
        }
        let head = h.head.load(Ordering::Relaxed);
        let tail = h.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= slots {
            return Pushed::Full;
        }
        let p = self.slot(head);
        unsafe {
            (p as *mut u32).write_unaligned(data.len() as u32);
            core::ptr::copy_nonoverlapping(data.as_ptr(), p.add(4), data.len());
        }
        h.head.store(head.wrapping_add(1), Ordering::Release);
        if head == tail || h.consumer_waiting.swap(0, Ordering::AcqRel) != 0 {
            Pushed::Wake
        } else {
            Pushed::Queued
        }
    }

    /// Consumer: take one message into `buf`; returns its length. When the
    /// ring is empty, the producer is asked to signal on the next push.
    pub fn pop(&self, buf: &mut [u8]) -> Option<usize> {
        let h = self.hdr();
        if h.slots.load(Ordering::Acquire) == 0 {
            return None;
        }
        let tail = h.tail.load(Ordering::Relaxed);
        if tail == h.head.load(Ordering::Acquire) {
            h.consumer_waiting.store(1, Ordering::Release);
            // Re-check: the producer may have pushed before seeing the flag.
            if tail == h.head.load(Ordering::Acquire) {
                return None;
            }
            h.consumer_waiting.store(0, Ordering::Relaxed);
        }
        let p = self.slot(tail);
        let n = (unsafe { (p as *const u32).read_unaligned() } as usize).min(buf.len());
        unsafe { core::ptr::copy_nonoverlapping(p.add(4), buf.as_mut_ptr(), n) };
        h.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(n)
    }
}

// ---- board ----

#[repr(C)]
struct BoardHeader {
    /// Odd while a write is in progress.
    seq: AtomicU64,
    len: AtomicU64,
}

const BOARD_HEADER: usize = 64;

/// A view of a board in memory: a 64-byte header, then the value.
pub struct Board {
    base: *mut u8,
    size: usize,
}

unsafe impl Send for Board {}

impl Board {
    /// # Safety
    /// `mem` must point to `size` bytes of memory shared only as this
    /// board, valid as long as the `Board` is used. One writer at a time.
    pub unsafe fn at(mem: *mut u8, size: usize) -> Board {
        Board { base: mem, size }
    }

    fn hdr(&self) -> &BoardHeader {
        unsafe { &*(self.base as *const BoardHeader) }
    }

    /// Largest value that fits.
    pub fn capacity(&self) -> usize {
        self.size - BOARD_HEADER
    }

    /// Writer: publish a new value. False if it doesn't fit.
    pub fn write(&self, data: &[u8]) -> bool {
        if data.len() > self.capacity() {
            return false;
        }
        let h = self.hdr();
        h.seq.fetch_add(1, Ordering::AcqRel);
        fence(Ordering::Release);
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr(), self.base.add(BOARD_HEADER), data.len())
        };
        h.len.store(data.len() as u64, Ordering::Relaxed);
        h.seq.fetch_add(1, Ordering::Release);
        true
    }

    /// Writer: publish `v` (encoded as every message). False if it doesn't
    /// fit.
    pub fn write_as<T: serde::Serialize>(&self, v: &T) -> bool {
        self.write(&crate::encode(v))
    }

    /// Reader: the value, if it changed since `*seq` (then updated) and
    /// decodes as a `T`. `buf` is kept between calls.
    pub fn read_as<T: serde::de::DeserializeOwned>(&self, seq: &mut u64, buf: &mut Vec<u8>) -> Option<T> {
        *seq = self.read(*seq, buf)?;
        crate::decode(buf)
    }

    /// The current version number (0 = nothing published yet).
    pub fn seq(&self) -> u64 {
        self.hdr().seq.load(Ordering::Acquire) & !1
    }

    /// Reader: copy the value out if it changed since `last_seq`. Returns
    /// the new sequence number. Gives up (None) if a writer keeps it busy.
    pub fn read(&self, last_seq: u64, out: &mut Vec<u8>) -> Option<u64> {
        let h = self.hdr();
        for _ in 0..100 {
            let s1 = h.seq.load(Ordering::Acquire);
            if s1 == last_seq || s1 == 0 {
                return None;
            }
            if s1 & 1 == 1 {
                core::hint::spin_loop();
                continue;
            }
            let len = (h.len.load(Ordering::Relaxed) as usize).min(self.capacity());
            out.clear();
            out.resize(len, 0);
            unsafe {
                core::ptr::copy_nonoverlapping(self.base.add(BOARD_HEADER), out.as_mut_ptr(), len)
            };
            fence(Ordering::Acquire);
            if h.seq.load(Ordering::Relaxed) == s1 {
                return Some(s1);
            }
        }
        None
    }
}

// ---- byte ring ----

/// Read/write positions of a [`ByteRing`] (free-running counters).
#[repr(C)]
pub struct ByteRingHdr {
    head: AtomicU32,
    tail: AtomicU32,
}

/// A single-producer single-consumer stream of bytes: the header lives in
/// one place, the data (a power-of-two size) in another.
pub struct ByteRing {
    hdr: *const ByteRingHdr,
    data: *mut u8,
    cap: u32,
}

unsafe impl Send for ByteRing {}

impl ByteRing {
    /// # Safety
    /// `hdr` and `data` (`cap` bytes, a power of two) must stay valid and be
    /// used only by this ring's one producer and one consumer.
    pub unsafe fn at(hdr: *mut u8, data: *mut u8, cap: u32) -> ByteRing {
        debug_assert!(cap.is_power_of_two());
        ByteRing {
            hdr: hdr as *const ByteRingHdr,
            data,
            cap,
        }
    }

    fn h(&self) -> &ByteRingHdr {
        unsafe { &*self.hdr }
    }

    pub fn len(&self) -> usize {
        let h = self.h();
        h.head
            .load(Ordering::Acquire)
            .wrapping_sub(h.tail.load(Ordering::Acquire)) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn free(&self) -> usize {
        self.cap as usize - self.len()
    }

    /// Producer: append as much of `data` as fits; returns how much.
    pub fn write(&self, data: &[u8]) -> usize {
        let h = self.h();
        let head = h.head.load(Ordering::Relaxed);
        let n = data.len().min(self.free());
        for (i, &b) in data[..n].iter().enumerate() {
            let at = head.wrapping_add(i as u32) & (self.cap - 1);
            unsafe { self.data.add(at as usize).write_volatile(b) };
        }
        h.head.store(head.wrapping_add(n as u32), Ordering::Release);
        n
    }

    /// Consumer: take up to `buf.len()` bytes; returns how many.
    pub fn read(&self, buf: &mut [u8]) -> usize {
        let h = self.h();
        let tail = h.tail.load(Ordering::Relaxed);
        let n = buf.len().min(self.len());
        for (i, b) in buf[..n].iter_mut().enumerate() {
            let at = tail.wrapping_add(i as u32) & (self.cap - 1);
            *b = unsafe { self.data.add(at as usize).read_volatile() };
        }
        h.tail.store(tail.wrapping_add(n as u32), Ordering::Release);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_fifo_full_and_wake() {
        let mut mem = vec![0u64; ring_size(4, 16) / 8];
        let r = unsafe { Ring::at(mem.as_mut_ptr().cast()) };
        let mut buf = [0u8; 16];
        assert_eq!(r.pop(&mut buf), None); // not set up yet
        r.init(4, 16);
        assert_eq!(r.push(b"a"), Pushed::Wake);
        assert_eq!(r.push(b"bb"), Pushed::Queued);
        assert_eq!(r.push(b"ccc"), Pushed::Queued);
        assert_eq!(r.push(b"dddd"), Pushed::Queued);
        assert_eq!(r.push(b"e"), Pushed::Full);
        assert_eq!(r.push(&[0; 17]), Pushed::Full);
        for want in [&b"a"[..], b"bb", b"ccc", b"dddd"] {
            let n = r.pop(&mut buf).unwrap();
            assert_eq!(&buf[..n], want);
        }
        assert_eq!(r.pop(&mut buf), None);
        // The consumer said it waits: the next push asks for a wake-up.
        assert_eq!(r.push(b"f"), Pushed::Wake);
        // Wraps around.
        for i in 0..100u8 {
            let n = r.pop(&mut buf).unwrap();
            assert_eq!(buf[0], if i == 0 { b'f' } else { i - 1 });
            assert_eq!(n, 1);
            r.push(&[i]);
        }
    }

    #[test]
    fn byte_ring() {
        let mut hdr = [0u64; 1];
        let mut data = [0u8; 8];
        let r = unsafe { ByteRing::at(hdr.as_mut_ptr().cast(), data.as_mut_ptr(), 8) };
        assert_eq!(r.write(b"hello world"), 8);
        let mut b = [0u8; 5];
        assert_eq!(r.read(&mut b), 5);
        assert_eq!(&b, b"hello");
        assert_eq!(r.write(b"12345"), 5); // wraps
        let mut b = [0u8; 16];
        assert_eq!(r.read(&mut b), 8);
        assert_eq!(&b[..8], b" wo12345");
        assert!(r.is_empty());
    }

    #[test]
    fn board_versions() {
        let mut mem = vec![0u64; 1024 / 8];
        let b = unsafe { Board::at(mem.as_mut_ptr().cast(), 1024) };
        let mut out = Vec::new();
        assert_eq!(b.read(0, &mut out), None);
        assert!(b.write(b"hello"));
        let s = b.read(0, &mut out).unwrap();
        assert_eq!(out, b"hello");
        assert_eq!(b.read(s, &mut out), None); // unchanged
        assert!(b.write(b"bye"));
        let s2 = b.read(s, &mut out).unwrap();
        assert!(s2 > s);
        assert_eq!(out, b"bye");
        assert!(!b.write(&[0; 1000]));
    }
}
