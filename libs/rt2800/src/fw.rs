//! The 8051 firmware (`run_load_microcode` in OpenBSD's if_run.c, plus
//! the image's CRC and the autorun check that Linux's rt2800usb does).
//!
//! The image is linux-firmware's `rt3071.bin`: 4 KiB, the same bytes as
//! the upper half of `rt2870.bin` (the half every chip after the
//! RT2870/RT3070 needs). Its last two bytes are a CRC-CCITT (reflected,
//! initial value 0xffff) of the rest, low byte first.

use touchos_rt::{exec, time};
use touchos_usb::HostController;

use crate::Error;
use crate::io::Io;
use crate::regs::*;

pub const FW_SIZE: usize = 4096;

/// The CRC the image's last two bytes should hold.
pub fn crc(data: &[u8]) -> u16 {
    let mut crc = 0xffffu16;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { crc >> 1 ^ 0x8408 } else { crc >> 1 };
        }
    }
    crc
}

/// Is `fw` a usable image (size and CRC)?
pub fn check(fw: &[u8]) -> Result<(), Error> {
    if fw.len() != FW_SIZE {
        return Err(Error::Firmware("not 4096 bytes (rt3071.bin is)"));
    }
    if crc(&fw[..FW_SIZE - 2]) != u16::from_le_bytes([fw[FW_SIZE - 2], fw[FW_SIZE - 1]]) {
        return Err(Error::Firmware("CRC mismatch"));
    }
    Ok(())
}

/// Where the image in the chip first differs from `fw` (None: the same).
async fn differs<H: HostController>(io: &mut Io<'_, H>, fw: &[u8]) -> Result<Option<usize>, Error> {
    let mut back = alloc::vec![0u8; FW_SIZE];
    io.read_region(FW_BASE, &mut back).await?;
    Ok(back.iter().zip(fw).position(|(a, b)| a != b))
}

/// How the firmware load went.
pub struct Loaded {
    /// The chip ran its firmware from ROM; nothing was uploaded.
    pub autorun: bool,
    /// The upload read back the same (None: not checked).
    pub verified: Option<bool>,
    /// The upload had to fall back to 16-bit writes.
    pub fallback: bool,
    /// Milliseconds from starting the MCU until it was ready.
    pub ready_ms: u64,
}

/// Upload and start the firmware.
pub async fn load<H: HostController>(io: &mut Io<'_, H>, fw: &[u8], autorun: bool) -> Result<Loaded, Error> {
    check(fw)?;
    // Linux wakes the chip first, so a reload (our server restarted
    // without a power cycle) doesn't hang.
    io.write(AUTO_WAKEUP_CFG, 0).await?;
    let mut verified = None;
    let mut fallback = false;
    if !autorun {
        io.write_region(FW_BASE, fw).await?;
        // Not in OpenBSD or Linux: read the image back. If MULTI_WRITE
        // didn't take, write it again the way OpenBSD does.
        match differs(io, fw).await {
            Ok(None) => verified = Some(true),
            Ok(Some(at)) => {
                ::log::warn!("rt2800: firmware read-back differs at {at:#x}; writing it again with 16-bit writes");
                io.write_region_2(FW_BASE, fw).await?;
                fallback = true;
                verified = differs(io, fw).await.ok().map(|d| d.is_none());
            }
            Err(e) => ::log::warn!("rt2800: firmware read-back failed ({e}); going on"),
        }
    }
    io.write(H2M_MAILBOX_CID, 0xffff_ffff).await?;
    io.write(H2M_MAILBOX_STATUS, 0xffff_ffff).await?;
    io.device_mode(MODE_FIRMWARE).await?;
    let start = time::now_ms();
    exec::sleep_ms(10).await;
    // Linux waits for the MCU here, OpenBSD after the boot signal below:
    // wait here (not fatal), and check again after it.
    let early = io.wait(SYS_CTRL, MCU_READY, MCU_READY, 250, 1000, "MCU ready").await;
    if early.is_err() {
        ::log::warn!("rt2800: MCU not ready after the firmware start; sending the boot signal anyway");
    }
    io.write(H2M_BBPAGENT, 0).await?;
    io.write(H2M_MAILBOX, 0).await?;
    io.write(H2M_INTSRC, 0).await?;
    io.mcu_cmd(MCU_CMD_RFRESET, 0).await?;
    io.wait(SYS_CTRL, MCU_READY, MCU_READY, 1000, 1000, "MCU ready").await?;
    Ok(Loaded { autorun, verified, fallback, ready_ms: time::now_ms() - start })
}
