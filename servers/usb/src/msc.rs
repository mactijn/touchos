//! USB mass storage (bulk-only transport, SCSI): disks on a USB port, such
//! as the SSD on the Toon's external port. **Read-only:** the wire formats
//! in touchos_usb::msc have no command that writes, and nothing here sends
//! one; the disk is identified, its partitions are listed, and it is read
//! (a speed test at attach, then a check every few minutes that the same
//! blocks read back the same, for duration tests).
//!
//! A command is the CBW out, the data in, the CSW in (`command`). A stall
//! on a data or status transfer is cleared (CLEAR_FEATURE(HALT)) and the
//! CSW read again, as the BOT spec says; anything worse gets the BOT reset
//! recovery.

use ::log::{info, warn};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::mem::{self, Dma};
use crate::{exec, time};
use touchos_blockdev::{BLOCK, BlockDev};
use touchos_usb::desc::Interface;
use touchos_usb::msc::{self, Status};
use touchos_usb::{Device, HostController, Result, UsbError};

/// The DMA buffer: the largest read in one command.
const BUF: usize = 64 * 1024;
/// One queued transfer at most (the EHCI's qTD limit).
const CHUNK: usize = 16 * 1024;
const CMD_TIMEOUT_MS: u64 = 10_000;
/// How long a disk may take to become ready (spin-up, unit attention).
const READY_MS: u64 = 15_000;
/// The speed test at attach.
const SPEED_TEST_BYTES: u64 = 32 << 20;
/// The periodic re-read check.
const CHECK_EVERY_MS: u64 = 5 * 60_000;
const CHECK_BYTES: usize = 4 << 20;

pub struct Msc {
    pub port: u8,
    iface: u8,
    ep_in: u8,
    ep_out: u8,
    tag: u32,
    buf: Dma,
    csw: Dma,
    pub blocks: u64,
    pub block_len: u32,
    next_check: u64,
    /// The periodic check's place and what it read there last time.
    check_lba: u64,
    check_crc: Option<u32>,
    pub reads: u64,
    pub errors: u64,
    /// A read failed: the device is re-attached (see main's `Recovery`).
    pub failed: bool,
}

impl Msc {
    /// Is this interface a BOT disk?
    pub fn is_msc(iface: &Interface) -> bool {
        iface.class == msc::CLASS && iface.subclass == msc::SUBCLASS_SCSI && iface.protocol == msc::PROTOCOL_BOT
    }

    pub async fn attach<H: HostController>(hc: &mut H, dev: &mut Device<H>, iface: &Interface) -> Result<Msc> {
        let bulk = |input: bool| iface.endpoints.iter().find(|e| e.transfer_type() == 2 && e.is_in() == input).copied();
        let (Some(ep_in), Some(ep_out)) = (bulk(true), bulk(false)) else {
            return Err(UsbError::Protocol("mass storage without bulk endpoints"));
        };
        dev.configure_endpoints(hc, &[ep_in, ep_out]).await?;
        // GET MAX LUN: many single-LUN devices stall it; LUN 0 is all we use.
        let luns = match dev.control_in(hc, 0xa1, msc::REQ_GET_MAX_LUN, 0, iface.number as u16, 1).await {
            Ok(1) => dev.data()[0] as u32 + 1,
            _ => 1,
        };
        let mut m = Msc {
            port: dev.port,
            iface: iface.number,
            ep_in: ep_in.address,
            ep_out: ep_out.address,
            tag: 1,
            buf: mem::alloc_dma(BUF),
            csw: mem::alloc_dma(64),
            blocks: 0,
            block_len: 0,
            next_check: 0,
            check_lba: 0,
            failed: false,
            check_crc: None,
            reads: 0,
            errors: 0,
        };
        let n = m.command(hc, dev, &msc::inquiry(), msc::INQUIRY_LEN as usize).await?;
        let q = msc::parse_inquiry(&m.buf.bytes()[..n]).ok_or(UsbError::Protocol("short INQUIRY answer"))?;
        if q.device_type != 0 {
            return Err(UsbError::Protocol("not a disk (SCSI peripheral type)"));
        }
        m.wait_ready(hc, dev).await?;
        let n = m.command(hc, dev, &msc::read_capacity10(), 8).await?;
        let cap = match msc::parse_capacity10(&m.buf.bytes()[..n]) {
            Some(c) => c,
            None => {
                let n = m.command(hc, dev, &msc::read_capacity16(), 32).await?;
                msc::parse_capacity16(&m.buf.bytes()[..n]).ok_or(UsbError::Protocol("bad READ CAPACITY answer"))?
            }
        };
        (m.blocks, m.block_len) = cap;
        if m.block_len as usize != BLOCK {
            return Err(UsbError::Protocol("block size isn't 512 bytes"));
        }
        info!(
            "port {} if{}: disk \"{} {}\" rev {}, {} blocks of {} bytes ({}), {luns} LUN{}{}; read-only",
            m.port,
            m.iface,
            q.vendor,
            q.product,
            q.revision,
            m.blocks,
            m.block_len,
            size(m.blocks * m.block_len as u64),
            if luns == 1 { "" } else { "s" },
            if q.removable { ", removable" } else { "" },
        );
        m.describe(hc, dev).await;
        m.speed_test(hc, dev).await;
        m.next_check = time::now_ms() + CHECK_EVERY_MS;
        Ok(m)
    }

    /// TEST UNIT READY until it passes (REQUEST SENSE after each failure
    /// clears a unit attention), or give up after [`READY_MS`].
    async fn wait_ready<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) -> Result<()> {
        let end = time::now_ms() + READY_MS;
        loop {
            match self.command(hc, dev, &msc::test_unit_ready(), 0).await {
                Ok(_) => return Ok(()),
                Err(UsbError::Protocol("command failed")) => {
                    if let Ok(n) = self.command(hc, dev, &msc::request_sense(), msc::SENSE_LEN as usize).await
                        && let Some((key, asc, ascq)) = msc::parse_sense(&self.buf.bytes()[..n])
                    {
                        ::log::debug!("port {}: not ready: sense key {key:#x}, ASC {asc:#04x}/{ascq:#04x}", self.port);
                    }
                }
                Err(e) => return Err(e),
            }
            if time::now_ms() > end {
                return Err(UsbError::Timeout);
            }
            exec::sleep_ms(100).await;
        }
    }

    /// Read `count` blocks at `lba` into the start of the DMA buffer.
    pub async fn read<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>, lba: u64, count: usize) -> Result<&[u8]> {
        let len = count * BLOCK;
        if count == 0 || len > BUF || lba + count as u64 > self.blocks {
            return Err(UsbError::Protocol("read outside the disk or buffer"));
        }
        let (cdb, n) = msc::read(lba, count as u16);
        let r = self.command(hc, dev, &cdb[..n], len).await;
        self.reads += 1;
        match r {
            Ok(got) if got == len => Ok(&self.buf.bytes()[..len]),
            Ok(_) => {
                self.errors += 1;
                Err(UsbError::Protocol("short read"))
            }
            Err(e) => {
                self.errors += 1;
                Err(e)
            }
        }
    }

    /// One BOT command with `data_len` bytes in (0: none). Returns the
    /// bytes received. A failed command (CSW status 1) is
    /// `Protocol("command failed")`.
    async fn command<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>, cdb: &[u8], data_len: usize) -> Result<usize> {
        let tag = self.tag;
        self.tag = self.tag.wrapping_add(1);
        let cbw = msc::cbw(tag, data_len as u32, true, 0, cdb);
        self.buf.bytes_mut()[..msc::CBW_LEN].copy_from_slice(&cbw);
        if let Err(e) = self.transfer(hc, dev, self.ep_out, self.buf.phys, msc::CBW_LEN).await {
            self.reset_recovery(hc, dev).await;
            return Err(e);
        }
        let mut got = 0;
        if data_len > 0 {
            match self.transfer(hc, dev, self.ep_in, self.buf.phys, data_len).await {
                Ok(n) => got = n,
                Err(UsbError::Stall) => self.clear_halt(hc, dev, self.ep_in).await,
                Err(e) => {
                    self.reset_recovery(hc, dev).await;
                    return Err(e);
                }
            }
        }
        // The CSW; a stall here is cleared and the CSW read once more.
        let mut csw = self.transfer(hc, dev, self.ep_in, self.csw.phys, msc::CSW_LEN).await;
        if csw == Err(UsbError::Stall) {
            self.clear_halt(hc, dev, self.ep_in).await;
            csw = self.transfer(hc, dev, self.ep_in, self.csw.phys, msc::CSW_LEN).await;
        }
        let parsed = match csw {
            Ok(n) => msc::parse_csw(&self.csw.bytes()[..n], tag),
            Err(_) => None,
        };
        match parsed {
            Some((Status::Passed, _)) => Ok(got),
            Some((Status::Failed, _)) => Err(UsbError::Protocol("command failed")),
            _ => {
                self.reset_recovery(hc, dev).await;
                Err(UsbError::Protocol("bad or missing CSW"))
            }
        }
    }

    /// Bulk transfer of `len` bytes at `phys` on `ep`, in chunks of
    /// [`CHUNK`], all queued at once; returns the bytes moved (a short
    /// packet ends it early).
    async fn transfer<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>, ep: u8, phys: u64, len: usize) -> Result<usize> {
        let mut tokens = Vec::new();
        let mut off = 0;
        while off < len {
            let n = (len - off).min(CHUNK);
            tokens.push((dev.queue(hc, ep, phys + off as u64, n as u32), n));
            off += n;
        }
        let end = time::now_ms() + CMD_TIMEOUT_MS;
        let mut moved = 0;
        let mut short = false;
        let mut err = None;
        for (token, n) in tokens {
            let r = loop {
                hc.clear_interrupts();
                if let Some(r) = dev.check(hc, ep, token) {
                    break r;
                }
                if time::now_ms() > end {
                    break Err(UsbError::Timeout);
                }
                // Sleeps on the controller's interrupt: a spin here kept
                // lower priorities off the CPU for the whole speed test.
                crate::wait_for_controller().await;
            };
            match r {
                Ok(left) if !short && err.is_none() => {
                    moved += n - left as usize;
                    short = left > 0;
                }
                Ok(_) => {}
                Err(e) => {
                    err.get_or_insert(e);
                }
            }
        }
        match err {
            Some(e) => Err(e),
            None => Ok(moved),
        }
    }

    async fn clear_halt<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>, ep: u8) {
        if let Err(e) = dev.control_out(hc, 0x02, 1, 0, ep as u16).await {
            warn!("port {}: clearing the halt of endpoint {ep:#04x} failed: {e}", self.port);
        }
    }

    /// BOT reset recovery: the class reset, then both endpoints' halts.
    async fn reset_recovery<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) {
        warn!("port {}: mass storage reset recovery", self.port);
        let _ = dev.control_out(hc, 0x21, msc::REQ_RESET, 0, self.iface as u16).await;
        self.clear_halt(hc, dev, self.ep_in).await;
        self.clear_halt(hc, dev, self.ep_out).await;
    }

    /// Log the partition table (MBR, or GPT checked by touchos_gpt) and
    /// what each partition's first block says it holds.
    async fn describe<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) {
        let mbr = match self.read(hc, dev, 0, 1).await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                warn!("port {}: reading block 0 failed: {e}", self.port);
                return;
            }
        };
        let Some(entries) = msc::mbr_entries(&mbr) else {
            info!("port {}: no partition table (no MBR signature)", self.port);
            return;
        };
        let parts: Vec<(String, u64, u64)> = if entries.iter().any(|e| e.0 == 0xee) {
            match self.gpt(hc, dev).await {
                Ok(t) => t.partitions.iter().map(|p| (String::from("GPT"), p.first_lba, p.blocks())).collect(),
                Err(e) => {
                    warn!("port {}: GPT: {e}", self.port);
                    return;
                }
            }
        } else {
            entries.iter().map(|&(t, first, n)| (format!("MBR type {t:#04x}"), first, n)).collect()
        };
        for (i, (kind, first, n)) in parts.iter().enumerate() {
            let fs = match self.read(hc, dev, *first, 1).await {
                Ok(b) => msc::fs_name(b),
                Err(_) => "unreadable",
            };
            info!("port {}: partition {}: {kind}, blocks {first}..{}, {}, {fs}", self.port, i + 1, first + n, size(n * BLOCK as u64));
        }
    }

    /// The GPT, read through a cache of the blocks touchos_gpt asks for
    /// (the protective MBR, both headers and both entry arrays).
    async fn gpt<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) -> core::result::Result<touchos_gpt::Table, String> {
        let last = self.blocks - 1;
        let mut cache = Cached { blocks: self.blocks, map: BTreeMap::new() };
        for (lba, n) in [(0u64, 34usize), (last - 32, 33)] {
            let b = self.read(hc, dev, lba, n).await.map_err(|e| format!("read: {e}"))?;
            for i in 0..n {
                cache.map.insert(lba + i as u64, b[i * BLOCK..(i + 1) * BLOCK].to_vec());
            }
        }
        touchos_gpt::read(&mut cache).map_err(|e| format!("{e:?}"))
    }

    /// Read [`SPEED_TEST_BYTES`] from the start of the disk, 64 KiB per
    /// command.
    async fn speed_test<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) {
        let per = BUF / BLOCK;
        let total = (SPEED_TEST_BYTES / BLOCK as u64).min(self.blocks);
        let t = time::now_us();
        let mut lba = 0;
        while lba < total {
            let n = per.min((total - lba) as usize);
            if let Err(e) = self.read(hc, dev, lba, n).await {
                warn!("port {}: speed test: read at {lba} failed: {e}", self.port);
                return;
            }
            lba += n as u64;
        }
        let us = (time::now_us() - t).max(1);
        info!(
            "port {}: read {} in {} ms: {} KB/s",
            self.port,
            size(total * BLOCK as u64),
            us / 1000,
            total * BLOCK as u64 * 1000 / us
        );
    }

    /// Every [`CHECK_EVERY_MS`]: read [`CHECK_BYTES`] at a place that moves
    /// across the disk, and compare with what the same place gave the
    /// time before (the place stays until it has been read twice).
    pub async fn check<H: HostController>(&mut self, hc: &mut H, dev: &mut Device<H>) {
        let now = time::now_ms();
        if now < self.next_check {
            return;
        }
        self.next_check = now + CHECK_EVERY_MS;
        let per = BUF / BLOCK;
        let count = CHECK_BYTES / BLOCK;
        let span = self.blocks.saturating_sub(count as u64).max(1);
        let lba = self.check_lba % span;
        let t = time::now_us();
        let mut crc = 0u32;
        let mut done = 0;
        while done < count {
            let n = per.min(count - done);
            match self.read(hc, dev, lba + done as u64, n).await {
                Ok(b) => crc = crc32(crc, b),
                Err(e) => {
                    warn!("port {}: check: read at {} failed: {e} ({} errors in {} reads)", self.port, lba + done as u64, self.errors, self.reads);
                    self.failed = true;
                    return;
                }
            }
            done += n;
        }
        let us = (time::now_us() - t).max(1);
        let rate = CHECK_BYTES as u64 * 1000 / us;
        match self.check_crc.take() {
            None => {
                self.check_crc = Some(crc);
                info!("port {}: check: {} at block {lba}: {rate} KB/s, CRC {crc:08x} ({} reads, {} errors)", self.port, size(CHECK_BYTES as u64), self.reads, self.errors);
            }
            Some(old) if old == crc => {
                info!("port {}: check: block {lba} reads back the same: {rate} KB/s ({} reads, {} errors)", self.port, self.reads, self.errors);
                self.check_lba = self.check_lba.wrapping_add(0x9e37_79b9) ; // the next place
            }
            Some(old) => {
                self.errors += 1;
                warn!("port {}: check: block {lba} read back DIFFERENT (CRC {old:08x}, now {crc:08x})", self.port);
                self.check_lba = self.check_lba.wrapping_add(0x9e37_79b9);
            }
        }
    }
}

/// The blocks read for the GPT; anything else is an error.
struct Cached {
    blocks: u64,
    map: BTreeMap<u64, Vec<u8>>,
}

impl BlockDev for Cached {
    fn blocks(&self) -> u64 {
        self.blocks
    }
    fn read(&mut self, lba: u64, buf: &mut [u8]) -> core::result::Result<(), touchos_blockdev::Error> {
        for (i, chunk) in buf.chunks_mut(BLOCK).enumerate() {
            let b = self.map.get(&(lba + i as u64)).ok_or(touchos_blockdev::Error::Io)?;
            chunk.copy_from_slice(b);
        }
        Ok(())
    }
    fn write(&mut self, _lba: u64, _buf: &[u8]) -> core::result::Result<(), touchos_blockdev::Error> {
        Err(touchos_blockdev::Error::ReadOnly)
    }
    fn flush(&mut self) -> core::result::Result<(), touchos_blockdev::Error> {
        Ok(())
    }
}

/// A size for the log: GB/MB/KB (decimal, as disks are sold).
fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1_000_000_000 => format!("{}.{} GB", b / 1_000_000_000, b % 1_000_000_000 / 100_000_000),
        b if b >= 1_000_000 => format!("{} MB", b / 1_000_000),
        b => format!("{} KB", b / 1000),
    }
}

/// CRC-32 (IEEE), continued from `crc`.
fn crc32(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xedb8_8320 } else { c >> 1 };
        }
    }
    !c
}
