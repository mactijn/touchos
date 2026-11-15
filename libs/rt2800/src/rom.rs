//! The calibration ROM: efuse or EEPROM (`run_read_eeprom`,
//! `run_efuse_read_2`, `run_eeprom_read_2` and `run_get_txpower` in
//! OpenBSD's if_run.c), the RT5390 family's words only, 2.4 GHz only.
//!
//! The ROM is read in 16-byte blocks (one efuse read gives a block) and
//! cached, so the ~40 words cost about 15 block reads.

use touchos_usb::HostController;

use crate::Error;
use crate::io::{Io, delay};
use crate::regs::*;

/// Words 0..0x100 (the 512-byte ROM), in blocks of 8.
const BLOCKS: usize = 32;

pub struct Rom {
    /// efuse (else an EEPROM, read with EEPROM_READ).
    pub efuse: bool,
    cache: [Option<[u16; 8]>; BLOCKS],
}

impl Rom {
    /// Which ROM the chip has: efuse if EFUSE_CTRL says so or the chip
    /// runs its firmware from ROM ("autorun", rt2800usb's rule).
    pub async fn detect<H: HostController>(io: &mut Io<'_, H>, autorun: bool) -> Result<Rom, Error> {
        let ctrl = io.read(EFUSE_CTRL).await?;
        Ok(Rom { efuse: autorun || ctrl & SEL_EFUSE != 0, cache: [None; BLOCKS] })
    }

    /// The 16-bit word at word address `addr`.
    pub async fn word<H: HostController>(&mut self, io: &mut Io<'_, H>, addr: u16) -> Result<u16, Error> {
        let b = (addr / 8) as usize;
        if b >= BLOCKS {
            return Ok(0xffff);
        }
        if self.cache[b].is_none() {
            let block = if self.efuse { efuse_block(io, b as u16 * 16).await? } else { eeprom_block(io, b as u16 * 16).await? };
            self.cache[b] = Some(block);
        }
        Ok(self.cache[b].unwrap()[(addr % 8) as usize])
    }
}

/// One efuse block: kick a read of the 16 bytes at `byte_addr`, then
/// EFUSE_DATA3 holds bytes 0-3, DATA2 bytes 4-7, ... DATA0 bytes 12-15.
async fn efuse_block<H: HostController>(io: &mut Io<'_, H>, byte_addr: u16) -> Result<[u16; 8], Error> {
    let mut ctrl = io.read(EFUSE_CTRL).await?;
    ctrl &= !(EFSROM_MODE_MASK | EFSROM_AIN_MASK);
    ctrl |= (byte_addr as u32 & !0xf) << EFSROM_AIN_SHIFT | EFSROM_KICK;
    io.write(EFUSE_CTRL, ctrl).await?;
    let mut done = None;
    for _ in 0..100 {
        let v = io.read(EFUSE_CTRL).await?;
        if v & EFSROM_KICK == 0 {
            done = Some(v);
            break;
        }
        delay(2).await;
    }
    let ctrl = done.ok_or(Error::Timeout("efuse read"))?;
    if ctrl & EFUSE_AOUT_MASK == EFUSE_AOUT_MASK {
        return Ok([0xffff; 8]); // not programmed
    }
    let mut w = [0u16; 8];
    for i in 0..4 {
        let v = io.read(EFUSE_DATA3 - 4 * i as u16).await?;
        w[2 * i] = v as u16;
        w[2 * i + 1] = (v >> 16) as u16;
    }
    Ok(w)
}

async fn eeprom_block<H: HostController>(io: &mut Io<'_, H>, byte_addr: u16) -> Result<[u16; 8], Error> {
    let mut b = [0u8; 16];
    io.eeprom_read(byte_addr, &mut b).await?;
    let mut w = [0u16; 8];
    for (i, c) in b.chunks(2).enumerate() {
        w[i] = u16::from_le_bytes([c[0], c[1]]);
    }
    Ok(w)
}

/// What the driver uses from the ROM, validated and defaulted as
/// OpenBSD does.
#[derive(Clone, Debug)]
pub struct Calib {
    pub efuse: bool,
    /// ROM version (low byte) and FAE number (high byte).
    pub version: u16,
    pub mac: [u8; 6],
    /// The RF chip (0x5370 for the RT5370).
    pub rf_rev: u16,
    pub ntx: u8,
    pub nrx: u8,
    /// Vendor BBP settings (register, value); 0 or 0xff registers unused.
    pub bbp: [(u8, u8); 10],
    /// RF frequency offset (crystal trim, RF R17).
    pub freq: u8,
    pub leds: u8,
    pub led: [u16; 3],
    pub patch_dac: bool,
    pub ext_2ghz_lna: bool,
    pub rfswitch: bool,
    /// TX power per channel 1-14 (RF R49).
    pub txpow1: [i8; 14],
    /// TX power per rate group at 20 MHz (TX_PWR_CFG0-4).
    pub txpow20mhz: [u32; 5],
    /// RSSI offsets of the 2.4 GHz band, per antenna.
    pub rssi_2ghz: [i8; 2],
    /// LNA gain of the 2.4 GHz band (channel group 0); 0xff (erased) is
    /// taken as 0.
    pub lna0: u8,
}

impl Calib {
    /// Read the words the RT5390 family uses (`run_read_eeprom`).
    pub async fn read<H: HostController>(io: &mut Io<'_, H>, rom: &mut Rom) -> Result<Calib, Error> {
        let version = rom.word(io, EEPROM_VERSION).await?;
        let mut mac = [0u8; 6];
        for i in 0..3 {
            let v = rom.word(io, EEPROM_MAC01 + i as u16).await?;
            mac[2 * i..2 * i + 2].copy_from_slice(&v.to_le_bytes());
        }
        // Vendor BBP settings (the RF ones at 0x82 are only for the RT3071's RF path).
        let mut bbp = [(0u8, 0u8); 10];
        for (i, b) in bbp.iter_mut().enumerate() {
            let v = rom.word(io, EEPROM_BBP_BASE + i as u16).await?;
            *b = ((v >> 8) as u8, v as u8);
        }
        let fl = rom.word(io, EEPROM_FREQ_LEDS).await?;
        let freq = if fl & 0xff != 0xff { fl as u8 } else { 0 };
        let (leds, led) = if fl >> 8 != 0xff {
            let mut led = [0u16; 3];
            for (i, l) in led.iter_mut().enumerate() {
                *l = rom.word(io, EEPROM_LED1 + i as u16).await?;
            }
            ((fl >> 8) as u8, led)
        } else {
            (0x01, [0x5555, 0x2221, 0x5627]) // broken ROM: the defaults
        };
        // The RF chip is word 0 on the RT5390 family; the chains are in
        // the antenna word.
        let chip = rom.word(io, EEPROM_CHIPID).await?;
        let (rf_rev, mut ntx, mut nrx) = if chip == 0xffff {
            (RF_5370, 1, 1) // OpenBSD defaults to a 1T1R RF here too
        } else {
            let ant = rom.word(io, EEPROM_ANTENNA).await?;
            (chip, ((ant >> 4) & 0xf) as u8, (ant & 0xf) as u8)
        };
        // Not in OpenBSD: an erased antenna word would mean 15 chains.
        if !(1..=2).contains(&ntx) {
            ntx = 1;
        }
        if !(1..=2).contains(&nrx) {
            nrx = 1;
        }
        let cfg = rom.word(io, EEPROM_CONFIG).await?;
        let patch_dac = cfg >> 8 != 0xff && (cfg >> 15) & 1 != 0;
        let (ext_2ghz_lna, rfswitch) = if cfg & 0xff != 0xff { ((cfg >> 2) & 1 != 0, cfg & 1 != 0) } else { (false, false) };
        // TX power for the 2.4 GHz channels (RT5390: one chain), fixed up
        // when out of range.
        let mut txpow1 = [0i8; 14];
        for i in (0..14).step_by(2) {
            let v = rom.word(io, EEPROM_PWR2GHZ_BASE1 + i as u16 / 2).await?;
            txpow1[i] = v as u8 as i8;
            txpow1[i + 1] = (v >> 8) as u8 as i8;
        }
        for p in txpow1.iter_mut() {
            if !(0..=27).contains(p) {
                *p = 5;
            }
        }
        let mut txpow20mhz = [0u32; 5];
        for (r, p) in txpow20mhz.iter_mut().enumerate() {
            let lo = rom.word(io, EEPROM_RPWR + 2 * r as u16).await?;
            let hi = rom.word(io, EEPROM_RPWR + 2 * r as u16 + 1).await?;
            *p = lo as u32 | (hi as u32) << 16;
        }
        let r = rom.word(io, EEPROM_RSSI1_2GHZ).await?;
        let mut rssi_2ghz = [r as u8 as i8, (r >> 8) as u8 as i8];
        for o in rssi_2ghz.iter_mut() {
            if !(-10..=10).contains(o) {
                *o = 0;
            }
        }
        let lna = rom.word(io, EEPROM_LNA).await?;
        Ok(Calib {
            efuse: rom.efuse,
            version,
            mac,
            rf_rev,
            ntx,
            nrx,
            bbp,
            freq,
            leds,
            led,
            patch_dac,
            ext_2ghz_lna,
            rfswitch,
            txpow1,
            txpow20mhz,
            rssi_2ghz,
            // Neither OpenBSD nor Linux checks this byte; an erased one
            // (0xff) would wreck the AGC start value and the RSSI.
            lna0: if lna as u8 == 0xff { 0 } else { lna as u8 },
        })
    }
}
