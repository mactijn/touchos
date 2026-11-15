//! Register access over USB vendor requests (`run_read`, `run_write`,
//! `run_bbp_read`, `run_rt3070_rf_read`, `run_mcu_cmd` in OpenBSD's
//! if_run.c), with retries on transfer errors as rt2x00 does.

use touchos_rt::{exec, time};
use touchos_usb::{Device, HostController, UsbError};

use crate::Error;
use crate::regs::*;

/// Attempts per vendor request (rt2x00 retries a failed request too).
const TRIES: u32 = 5;
/// MULTI_WRITE/MULTI_READ chunk size (rt2x00's CSR cache is 64 bytes).
const CHUNK: usize = 64;

/// The chip's registers, through EP0 of its USB device.
pub struct Io<'a, H: HostController> {
    pub hc: &'a mut H,
    pub dev: &'a mut Device<H>,
    /// Control transfers done (for the init statistics).
    pub transfers: u32,
}

impl<'a, H: HostController> Io<'a, H> {
    pub fn new(hc: &'a mut H, dev: &'a mut Device<H>) -> Self {
        Io { hc, dev, transfers: 0 }
    }

    /// A vendor IN request; the data is in `self.dev.data()`.
    async fn vendor_in(&mut self, req: u8, value: u16, index: u16, len: u16) -> Result<usize, Error> {
        let mut last = UsbError::Timeout;
        for _ in 0..TRIES {
            self.transfers += 1;
            match self.dev.control_in(self.hc, 0x40, req, value, index, len).await {
                Ok(n) => return Ok(n),
                Err(e) => last = e,
            }
        }
        Err(Error::Usb(req, index, last))
    }

    /// A vendor OUT request, with `data` (or none).
    async fn vendor_out(&mut self, req: u8, value: u16, index: u16, data: &[u8]) -> Result<(), Error> {
        let mut last = UsbError::Timeout;
        for _ in 0..TRIES {
            self.transfers += 1;
            let r = if data.is_empty() {
                self.dev.control_out(self.hc, 0x40, req, value, index).await
            } else {
                self.dev.control_out_data(self.hc, 0x40, req, value, index, data).await
            };
            match r {
                Ok(()) => return Ok(()),
                Err(e) => last = e,
            }
        }
        Err(Error::Usb(req, index, last))
    }

    /// A 32-bit register (MULTI_READ of 4 bytes).
    pub async fn read(&mut self, reg: u16) -> Result<u32, Error> {
        let n = self.vendor_in(REQ_MULTI_READ, 0, reg, 4).await?;
        if n < 4 {
            return Err(Error::Chip("short register read"));
        }
        let d = self.dev.data();
        Ok(u32::from_le_bytes([d[0], d[1], d[2], d[3]]))
    }

    /// A 16-bit write (SINGLE_WRITE).
    pub async fn write_2(&mut self, reg: u16, val: u16) -> Result<(), Error> {
        self.vendor_out(REQ_WRITE_2, val, reg, &[]).await
    }

    /// A 32-bit register, as two 16-bit writes (as OpenBSD does).
    pub async fn write(&mut self, reg: u16, val: u32) -> Result<(), Error> {
        self.write_2(reg, val as u16).await?;
        self.write_2(reg + 2, (val >> 16) as u16).await
    }

    /// Bytes to consecutive registers (MULTI_WRITE, in 64-byte requests).
    pub async fn write_region(&mut self, reg: u16, data: &[u8]) -> Result<(), Error> {
        for (i, c) in data.chunks(CHUNK).enumerate() {
            self.vendor_out(REQ_MULTI_WRITE, 0, reg + (i * CHUNK) as u16, c).await?;
        }
        Ok(())
    }

    /// Bytes to consecutive registers as 16-bit writes: OpenBSD's way
    /// (it found MULTI_WRITE unreliable on the RT2860).
    pub async fn write_region_2(&mut self, reg: u16, data: &[u8]) -> Result<(), Error> {
        for (i, c) in data.chunks(2).enumerate() {
            let v = c[0] as u16 | (*c.get(1).unwrap_or(&0) as u16) << 8;
            self.write_2(reg + 2 * i as u16, v).await?;
        }
        Ok(())
    }

    /// `count` 32-bit registers from `reg` on set to 0 (`run_set_region_4`).
    pub async fn zero_region(&mut self, reg: u16, count: usize) -> Result<(), Error> {
        let zeros = [0u8; CHUNK];
        let mut left = count * 4;
        let mut at = reg;
        while left > 0 {
            let n = left.min(CHUNK);
            self.vendor_out(REQ_MULTI_WRITE, 0, at, &zeros[..n]).await?;
            at = at.wrapping_add(n as u16);
            left -= n;
        }
        Ok(())
    }

    /// Bytes from consecutive registers (MULTI_READ, in 64-byte requests).
    pub async fn read_region(&mut self, reg: u16, out: &mut [u8]) -> Result<(), Error> {
        for (i, c) in out.chunks_mut(CHUNK).enumerate() {
            let n = self.vendor_in(REQ_MULTI_READ, 0, reg + (i * CHUNK) as u16, c.len() as u16).await?;
            if n < c.len() {
                return Err(Error::Chip("short region read"));
            }
            c.copy_from_slice(&self.dev.data()[..c.len()]);
        }
        Ok(())
    }

    /// EEPROM bytes (EEPROM_READ), for a chip without efuse.
    pub async fn eeprom_read(&mut self, byte_addr: u16, out: &mut [u8]) -> Result<(), Error> {
        let n = self.vendor_in(REQ_EEPROM_READ, 0, byte_addr, out.len() as u16).await?;
        if n < out.len() {
            return Err(Error::Chip("short EEPROM read"));
        }
        out.copy_from_slice(&self.dev.data()[..out.len()]);
        Ok(())
    }

    /// DEVICE_MODE: reset (1) or start the firmware (8).
    pub async fn device_mode(&mut self, mode: u16) -> Result<(), Error> {
        self.vendor_out(REQ_DEVICE_MODE, mode, 0, &[]).await
    }

    /// Read `reg` until `(value & mask) == want`, up to `tries` times with
    /// `delay_us` between (sleeping for a millisecond or more).
    pub async fn wait(&mut self, reg: u16, mask: u32, want: u32, tries: u32, delay_us: u64, what: &'static str) -> Result<u32, Error> {
        for _ in 0..tries {
            let v = self.read(reg).await?;
            if v & mask == want {
                return Ok(v);
            }
            delay(delay_us).await;
        }
        Err(Error::Timeout(what))
    }

    /// Wait until the BBP/RF access register's kick bit is clear.
    async fn idle(&mut self, reg: u16, kick: u32, tries: u32, what: &'static str) -> Result<u32, Error> {
        for _ in 0..tries {
            let v = self.read(reg).await?;
            if v & kick == 0 {
                return Ok(v);
            }
        }
        Err(Error::Timeout(what))
    }

    /// A baseband register, through BBP_CSR_CFG.
    pub async fn bbp_read(&mut self, reg: u8) -> Result<u8, Error> {
        self.idle(BBP_CSR_CFG, BBP_CSR_KICK, 10, "BBP idle").await?;
        self.write(BBP_CSR_CFG, BBP_CSR_READ | BBP_CSR_KICK | (reg as u32) << 8).await?;
        let v = self.idle(BBP_CSR_CFG, BBP_CSR_KICK, 10, "BBP read").await?;
        Ok(v as u8)
    }

    pub async fn bbp_write(&mut self, reg: u8, val: u8) -> Result<(), Error> {
        self.idle(BBP_CSR_CFG, BBP_CSR_KICK, 10, "BBP idle").await?;
        self.write(BBP_CSR_CFG, BBP_CSR_KICK | (reg as u32) << 8 | val as u32).await
    }

    /// An RF register (RFCSR), through RF_CSR_CFG.
    pub async fn rf_read(&mut self, reg: u8) -> Result<u8, Error> {
        self.idle(RF_CSR_CFG, RF_KICK, 100, "RF idle").await?;
        self.write(RF_CSR_CFG, RF_KICK | (reg as u32) << 8).await?;
        let v = self.idle(RF_CSR_CFG, RF_KICK, 100, "RF read").await?;
        Ok(v as u8)
    }

    pub async fn rf_write(&mut self, reg: u8, val: u8) -> Result<(), Error> {
        self.idle(RF_CSR_CFG, RF_KICK, 10, "RF idle").await?;
        self.write(RF_CSR_CFG, RF_WRITE | RF_KICK | (reg as u32) << 8 | val as u32).await
    }

    /// A command to the chip's 8051 (through the host-to-MCU mailbox).
    pub async fn mcu_cmd(&mut self, cmd: u8, arg: u16) -> Result<(), Error> {
        self.idle(H2M_MAILBOX, H2M_BUSY, 100, "MCU mailbox").await?;
        self.write(H2M_MAILBOX, H2M_BUSY | TOKEN_NO_INTR << 16 | arg as u32).await?;
        self.write(HOST_CMD, cmd as u32).await
    }
}

/// OpenBSD's DELAY(): microseconds busy-waited, milliseconds slept.
pub async fn delay(us: u64) {
    if us >= 1000 {
        exec::sleep_ms(us / 1000).await;
    } else {
        time::busy_wait_us(us);
    }
}
