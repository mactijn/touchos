//! The i.MX I²C controller (i.MX21/25/27 "imx-i2c"), as a polled master.
//! Registers are 16 bits wide, 4 bytes apart.

use touchos_drv::mmio::{Regs, poll_until};
use touchos_rt::time;

const IFDR: usize = 0x04;
const I2CR: usize = 0x08;
const I2SR: usize = 0x0c;
const I2DR: usize = 0x10;

const CR_IEN: u16 = 0x80;
const CR_MSTA: u16 = 0x20;
const CR_MTX: u16 = 0x10;
const CR_TXAK: u16 = 0x08;
const CR_RSTA: u16 = 0x04;

const SR_ICF: u16 = 0x80; // the byte transfer is complete
const SR_IBB: u16 = 0x20;
const SR_IAL: u16 = 0x10;
const SR_IIF: u16 = 0x02;
const SR_RXAK: u16 = 0x01;

/// What went wrong in a transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No acknowledge (no device at the address, or it refused a byte).
    Nack,
    ArbitrationLost,
    /// The controller didn't finish a step in time (bus stuck?).
    Timeout,
}

pub struct I2c {
    regs: Regs,
}

impl I2c {
    /// The controller whose registers the MMIO capability `regs` names.
    /// `ifdr` selects the bus clock divider (see the reference manual's
    /// table: 0x38 divides by 640, 0x1f by the most).
    pub fn new(regs: u32, ifdr: u16) -> Option<I2c> {
        let i = I2c {
            regs: Regs::from_cap(regs, 0, 0x1000).ok()?,
        };
        i.regs.w16(I2CR, 0);
        i.regs.w16(IFDR, ifdr);
        i.regs.w16(I2CR, CR_IEN);
        i.regs.w16(I2SR, 0);
        Some(i)
    }

    /// Poll the status register until `done` (2 ms at most: a byte takes
    /// well under 1 ms at the slowest clock we use). The deadline is wall
    /// time and we can be preempted for a whole tick (4 ms): look once more
    /// after it passed, so a step that finished meanwhile isn't a timeout.
    fn wait(&self, done: impl Fn(u16) -> bool) -> Result<u16, Error> {
        poll_until(time::now_us, 2000, || {
            let sr = self.regs.r16(I2SR);
            done(sr).then_some(sr)
        })
        .ok_or(Error::Timeout)
    }

    /// Send one byte; it must be acknowledged.
    fn send(&self, b: u8) -> Result<(), Error> {
        self.regs.w16(I2DR, b as u16);
        // (QEMU doesn't raise IIF when no device answers an address, only
        // RXAK; the hardware raises both.) RXAK keeps the last byte's
        // answer, so it only counts once this byte is complete (ICF): else
        // it is a timeout, which probes retry.
        let sr = self.wait(|sr| sr & SR_IIF != 0).map_err(|e| {
            let sr = self.regs.r16(I2SR);
            if sr & (SR_RXAK | SR_ICF) == SR_RXAK | SR_ICF { Error::Nack } else { e }
        })?;
        self.regs.w16(I2SR, 0);
        if sr & SR_IAL != 0 {
            return Err(Error::ArbitrationLost);
        }
        if sr & SR_RXAK != 0 {
            return Err(Error::Nack);
        }
        Ok(())
    }

    fn stop(&self) {
        self.regs.w16(I2CR, CR_IEN);
        let _ = self.wait(|sr| sr & SR_IBB == 0);
    }

    /// [`xfer`](Self::xfer), tried up to three times unless the device says
    /// no (Nack): for probes, which decide once whether a device is there.
    pub fn xfer_probe(&self, addr: u8, wr: &[u8], rd: &mut [u8]) -> Result<(), Error> {
        let mut r = self.xfer(addr, wr, rd);
        for _ in 0..2 {
            if !matches!(r, Err(Error::Timeout | Error::ArbitrationLost)) {
                break;
            }
            r = self.xfer(addr, wr, rd);
        }
        r
    }

    /// Write `wr` to the device at `addr` (7 bits), then read `rd` from it
    /// after a repeated start. Either may be empty.
    pub fn xfer(&self, addr: u8, wr: &[u8], rd: &mut [u8]) -> Result<(), Error> {
        let r = self.xfer_inner(addr, wr, rd);
        if r.is_err() {
            // Release the bus and start over clean.
            self.stop();
            self.regs.w16(I2CR, 0);
            self.regs.w16(I2CR, CR_IEN);
            self.regs.w16(I2SR, 0);
        }
        r
    }

    fn xfer_inner(&self, addr: u8, wr: &[u8], rd: &mut [u8]) -> Result<(), Error> {
        self.wait(|sr| sr & SR_IBB == 0)?;
        self.regs.w16(I2SR, 0);
        // Start: become master, transmitting.
        self.regs.w16(I2CR, CR_IEN | CR_MSTA | CR_MTX);
        self.wait(|sr| sr & SR_IBB != 0)?;
        if !wr.is_empty() || rd.is_empty() {
            self.send(addr << 1)?;
            for &b in wr {
                self.send(b)?;
            }
            if rd.is_empty() {
                self.stop();
                return Ok(());
            }
            self.regs.w16(I2CR, CR_IEN | CR_MSTA | CR_MTX | CR_RSTA);
        }
        self.send(addr << 1 | 1)?;
        // Receive. TXAK: don't acknowledge the last byte. Reading I2DR
        // starts the next byte, so the first read only starts the transfer,
        // and before the last read the controller must be told to stop.
        let n = rd.len();
        self.regs.w16(I2CR, CR_IEN | CR_MSTA | if n == 1 { CR_TXAK } else { 0 });
        let _ = self.regs.r16(I2DR);
        for i in 0..n {
            self.wait(|sr| sr & SR_IIF != 0)?;
            self.regs.w16(I2SR, 0);
            if i == n - 1 {
                self.regs.w16(I2CR, CR_IEN); // stop
            } else if i == n - 2 {
                self.regs.w16(I2CR, CR_IEN | CR_MSTA | CR_TXAK);
            }
            rd[i] = self.regs.r16(I2DR) as u8;
        }
        self.wait(|sr| sr & SR_IBB == 0)?;
        Ok(())
    }
}
