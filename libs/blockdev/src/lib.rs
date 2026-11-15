//! Block devices: 512-byte blocks addressed by LBA.
//!
//! - [`BlockDev`]: what a driver (SDHCI) provides.
//! - [`Window`]: a partition seen from inside: LBA 0 is its first block and
//!   nothing outside it can be reached (the FAT code only ever gets one).
//! - With the `testing` feature: [`MemDisk`], [`CrashAfter`] (power loss
//!   after N writes) and [`Audit`] (records every write).

#![cfg_attr(not(any(test, feature = "testing")), no_std)]

extern crate alloc;

pub const BLOCK: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The device reported an error or timed out.
    Io,
    /// Outside the device or window.
    OutOfRange,
    /// Writing is not allowed (read-only mode, or outside the write fence).
    ReadOnly,
    /// The buffer is not a whole number of blocks.
    BadLength,
}

pub trait BlockDev {
    /// Number of blocks.
    fn blocks(&self) -> u64;
    /// Read `buf.len() / 512` blocks from `lba`.
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Error>;
    /// Write `buf.len() / 512` blocks at `lba`.
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), Error>;
    /// Like `write`, but all-or-nothing if the device can do that (eMMC
    /// reliable write); used for the one block that commits a change.
    fn write_reliable(&mut self, lba: u64, buf: &[u8]) -> Result<(), Error> {
        self.write(lba, buf)
    }
    /// Make earlier writes durable (empty the device's write cache).
    fn flush(&mut self) -> Result<(), Error>;
}

impl<T: BlockDev + ?Sized> BlockDev for &mut T {
    fn blocks(&self) -> u64 {
        (**self).blocks()
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Error> {
        (**self).read(lba, buf)
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), Error> {
        (**self).write(lba, buf)
    }
    fn write_reliable(&mut self, lba: u64, buf: &[u8]) -> Result<(), Error> {
        (**self).write_reliable(lba, buf)
    }
    fn flush(&mut self) -> Result<(), Error> {
        (**self).flush()
    }
}

fn check(lba: u64, len: usize, blocks: u64) -> Result<u64, Error> {
    if len % BLOCK != 0 {
        return Err(Error::BadLength);
    }
    let n = (len / BLOCK) as u64;
    match lba.checked_add(n) {
        Some(end) if end <= blocks => Ok(n),
        _ => Err(Error::OutOfRange),
    }
}

/// A partition: blocks `[start, start + len)` of the device, seen as `0..len`.
pub struct Window<D> {
    dev: D,
    start: u64,
    len: u64,
}

impl<D: BlockDev> Window<D> {
    pub fn new(dev: D, start: u64, len: u64) -> Result<Window<D>, Error> {
        // In blocks, not bytes: a partition of 4 GiB or more doesn't fit
        // a 32-bit usize in bytes.
        match start.checked_add(len) {
            Some(end) if end <= dev.blocks() => {}
            _ => return Err(Error::OutOfRange),
        }
        Ok(Window { dev, start, len })
    }
    pub fn into_inner(self) -> D {
        self.dev
    }
}

impl<D: BlockDev> BlockDev for Window<D> {
    fn blocks(&self) -> u64 {
        self.len
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Error> {
        check(lba, buf.len(), self.len)?;
        self.dev.read(self.start + lba, buf)
    }
    fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), Error> {
        check(lba, buf.len(), self.len)?;
        self.dev.write(self.start + lba, buf)
    }
    fn write_reliable(&mut self, lba: u64, buf: &[u8]) -> Result<(), Error> {
        check(lba, buf.len(), self.len)?;
        self.dev.write_reliable(self.start + lba, buf)
    }
    fn flush(&mut self) -> Result<(), Error> {
        self.dev.flush()
    }
}

#[cfg(any(test, feature = "testing"))]
pub use testing::*;

#[cfg(any(test, feature = "testing"))]
mod testing {
    use super::*;
    use alloc::vec::Vec;

    /// A disk in memory.
    #[derive(Clone)]
    pub struct MemDisk(pub Vec<u8>);

    impl BlockDev for MemDisk {
        fn blocks(&self) -> u64 {
            (self.0.len() / BLOCK) as u64
        }
        fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Error> {
            check(lba, buf.len(), self.blocks())?;
            let o = lba as usize * BLOCK;
            buf.copy_from_slice(&self.0[o..o + buf.len()]);
            Ok(())
        }
        fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), Error> {
            check(lba, buf.len(), self.blocks())?;
            let o = lba as usize * BLOCK;
            self.0[o..o + buf.len()].copy_from_slice(buf);
            Ok(())
        }
        fn flush(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }

    /// Power loss after `n` block writes: later writes fail and change
    /// nothing. (Each block counts; a multi-block write may be cut in the
    /// middle, like a real disk.)
    pub struct CrashAfter<D> {
        pub inner: D,
        pub left: usize,
    }

    impl<D: BlockDev> BlockDev for CrashAfter<D> {
        fn blocks(&self) -> u64 {
            self.inner.blocks()
        }
        fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Error> {
            self.inner.read(lba, buf)
        }
        fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), Error> {
            for (i, b) in buf.chunks(BLOCK).enumerate() {
                if self.left == 0 {
                    return Err(Error::Io);
                }
                self.left -= 1;
                self.inner.write(lba + i as u64, b)?;
            }
            Ok(())
        }
        fn flush(&mut self) -> Result<(), Error> {
            if self.left == 0 {
                Err(Error::Io)
            } else {
                Ok(())
            }
        }
    }

    /// Records the LBA of every block written (and counts them).
    pub struct Audit<D> {
        pub inner: D,
        pub written: Vec<u64>,
        pub reliable: Vec<u64>,
        pub flushes: usize,
    }

    impl<D: BlockDev> Audit<D> {
        pub fn new(inner: D) -> Audit<D> {
            Audit {
                inner,
                written: Vec::new(),
                reliable: Vec::new(),
                flushes: 0,
            }
        }
    }

    impl<D: BlockDev> BlockDev for Audit<D> {
        fn blocks(&self) -> u64 {
            self.inner.blocks()
        }
        fn read(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), Error> {
            self.inner.read(lba, buf)
        }
        fn write(&mut self, lba: u64, buf: &[u8]) -> Result<(), Error> {
            for i in 0..(buf.len() / BLOCK) as u64 {
                self.written.push(lba + i);
            }
            self.inner.write(lba, buf)
        }
        fn write_reliable(&mut self, lba: u64, buf: &[u8]) -> Result<(), Error> {
            for i in 0..(buf.len() / BLOCK) as u64 {
                self.written.push(lba + i);
                self.reliable.push(lba + i);
            }
            self.inner.write_reliable(lba, buf)
        }
        fn flush(&mut self) -> Result<(), Error> {
            self.flushes += 1;
            self.inner.flush()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_bounds() {
        let disk = MemDisk(alloc::vec![0; 100 * BLOCK]);
        let mut w = Window::new(disk, 10, 20).unwrap();
        let b = [7u8; BLOCK];
        w.write(0, &b).unwrap();
        w.write(19, &b).unwrap();
        assert_eq!(w.write(20, &b), Err(Error::OutOfRange));
        assert_eq!(w.write(19, &[0; 2 * BLOCK]), Err(Error::OutOfRange));
        assert_eq!(w.write(0, &[0; 100]), Err(Error::BadLength));
        let disk = w.into_inner();
        assert_eq!(disk.0[10 * BLOCK], 7);
        assert_eq!(disk.0[29 * BLOCK], 7);
        assert_eq!(disk.0[30 * BLOCK], 0);
        assert!(Window::new(disk, 90, 11).is_err());
    }

    #[test]
    fn crash_after() {
        let mut d = CrashAfter {
            inner: MemDisk(alloc::vec![0; 4 * BLOCK]),
            left: 3,
        };
        assert_eq!(d.write(0, &[1; 2 * BLOCK]), Ok(()));
        assert_eq!(d.write(2, &[1; 2 * BLOCK]), Err(Error::Io));
        assert_eq!(d.inner.0[2 * BLOCK], 1);
        assert_eq!(d.inner.0[3 * BLOCK], 0);
    }
}
