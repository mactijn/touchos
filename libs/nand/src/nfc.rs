//! The i.MX27's NAND flash controller, NFC version 1 (as Linux's
//! `mxc_nand` calls it: i.MX21/27), driving the Toon's Samsung K9F1G08
//! (ID `ec f1`: 128 MiB, 2 KiB pages, 64 pages a block, 8-bit). Polled.
//!
//! The NFC has a 2 KiB + 64 byte RAM buffer in front of the chip, and
//! moves data in 512-byte subpages: a 2 KiB page is four data-out (or
//! data-in) operations, buffer 0 to 3, each with its 16 spare bytes. On
//! the chip a page is laid out the same way: 512 bytes of data, 16 spare,
//! four times. The hardware ECC (1 bit per subpage corrected, 2 detected)
//! sits in the spare bytes; the NFC corrects the buffer as it reads.
//!
//! The NFC waits for the chip's ready/busy line itself: every operation
//! is "start it, wait for the INT bit in CONFIG2". Register sequences after
//! Linux's mxc_nand.c (v1 paths).
//!
//! Bad blocks are recognised as U-Boot and stock Linux do on this board:
//! by the first spare byte of buffer 0 (the NFC's view of "OOB byte 0") of
//! the block's first or second page.

/// Bytes in a page (data), its spare area, pages in a block.
pub const PAGE: usize = 2048;
pub const SPARE: usize = 64;
pub const PAGES_PER_BLOCK: u32 = 64;
pub const BLOCK: usize = PAGE * PAGES_PER_BLOCK as usize;
pub const BLOCKS: u32 = 1024;
/// The chip's ID bytes (maker, device).
pub const ID: [u8; 2] = [0xec, 0xf1];

// The RAM buffer: main areas (4 × 512) and spare areas (4 × 16).
const MAIN: usize = 0x000;
const SPARE0: usize = 0x800;
// Registers (16 bits each).
const BUF_ADDR: usize = 0xe04;
const FLASH_ADDR: usize = 0xe06;
const FLASH_CMD: usize = 0xe08;
const CONFIG: usize = 0xe0a;
const ECC_STATUS: usize = 0xe0c;
const WRPROT: usize = 0xe12;
const UNLOCK_START: usize = 0xe14;
const UNLOCK_END: usize = 0xe16;
const CONFIG1: usize = 0xe1a;
const CONFIG2: usize = 0xe1c;

const CONFIG1_ECC_EN: u16 = 1 << 3;
const CONFIG1_INT_MSK: u16 = 1 << 4;
const CONFIG2_INT: u16 = 1 << 15;
const OP_CMD: u16 = 1 << 0;
const OP_ADDR: u16 = 1 << 1;
const OP_INPUT: u16 = 1 << 2;
const OP_OUTPUT: u16 = 1 << 3;
const OP_ID: u16 = 1 << 4;
const OP_STATUS: u16 = 1 << 5;

const CMD_READ0: u8 = 0x00;
const CMD_READSTART: u8 = 0x30;
const CMD_SEQIN: u8 = 0x80;
const CMD_PAGEPROG: u8 = 0x10;
const CMD_ERASE1: u8 = 0x60;
const CMD_ERASE2: u8 = 0xd0;
const CMD_STATUS: u8 = 0x70;
const CMD_READID: u8 = 0x90;
const CMD_RESET: u8 = 0xff;
/// Status: the last program or erase failed.
const STATUS_FAIL: u8 = 1 << 0;

/// How long an operation may take, counted in polls of CONFIG2. An erase
/// takes up to 3 ms (2 ms typical), a program 700 µs; a poll is a few
/// hundred ns, so this is far beyond either.
const POLLS: u32 = 2_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The NFC never signalled the end of an operation.
    Timeout,
    /// More bit errors in a subpage than the ECC corrects.
    Ecc,
    /// The chip reported a failed program or erase (the block is wearing
    /// out: mark it bad).
    Failed,
    /// A page or block beyond the chip (or below the first one touchos
    /// may write).
    Range,
    /// What was written didn't read back the same.
    Verify,
}

/// How a page read went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Read {
    /// Subpages in which the ECC corrected a bit.
    pub corrected: u8,
}

/// The NFC at `base` (its 4 KiB page: buffer and registers).
pub struct Nfc {
    base: usize,
}

impl Nfc {
    /// # Safety
    /// `base` maps the NFC's page (physical 0xd8000000), uncached, and no
    /// one else drives the controller.
    pub unsafe fn new(base: usize) -> Nfc {
        Nfc { base }
    }

    fn r16(&self, off: usize) -> u16 {
        unsafe { ((self.base + off) as *const u16).read_volatile() }
    }
    fn w16(&self, off: usize, v: u16) {
        unsafe { ((self.base + off) as *mut u16).write_volatile(v) }
    }
    fn r32(&self, off: usize) -> u32 {
        unsafe { ((self.base + off) as *const u32).read_volatile() }
    }
    fn w32(&self, off: usize, v: u32) {
        unsafe { ((self.base + off) as *mut u32).write_volatile(v) }
    }

    /// Wait for the operation just started, and acknowledge it.
    fn wait(&self) -> Result<(), Error> {
        for _ in 0..POLLS {
            let c = self.r16(CONFIG2);
            if c & CONFIG2_INT != 0 {
                self.w16(CONFIG2, c & !CONFIG2_INT);
                return Ok(());
            }
        }
        Err(Error::Timeout)
    }

    fn op(&self, op: u16) -> Result<(), Error> {
        self.w16(CONFIG2, op);
        self.wait()
    }

    fn cmd(&self, c: u8) -> Result<(), Error> {
        self.w16(FLASH_CMD, c as u16);
        self.op(OP_CMD)
    }

    fn addr(&self, a: u8) -> Result<(), Error> {
        self.w16(FLASH_ADDR, a as u16);
        self.op(OP_ADDR)
    }

    /// Column 0 and the page's row (2 + 2 cycles: the chip is 128 MiB).
    fn page_addr(&self, page: u32) -> Result<(), Error> {
        self.addr(0)?;
        self.addr(0)?;
        self.row_addr(page)
    }

    fn row_addr(&self, page: u32) -> Result<(), Error> {
        self.addr(page as u8)?;
        self.addr((page >> 8) as u8)
    }

    /// Unlock the RAM buffer and every block, polled (interrupt masked),
    /// hardware ECC on; then reset the chip. U-Boot has set up the clock
    /// and the pins.
    pub fn init(&self) -> Result<(), Error> {
        self.w16(CONFIG, 2);
        self.w16(UNLOCK_START, 0);
        self.w16(UNLOCK_END, 0x4000);
        self.w16(WRPROT, 4);
        self.w16(CONFIG1, CONFIG1_ECC_EN | CONFIG1_INT_MSK);
        self.cmd(CMD_RESET)
    }

    /// The chip's ID bytes (maker, device, and two more).
    pub fn read_id(&self) -> Result<[u8; 4], Error> {
        self.cmd(CMD_READID)?;
        self.addr(0)?;
        self.w16(BUF_ADDR, 0);
        self.op(OP_ID)?;
        Ok(self.r32(MAIN).to_le_bytes())
    }

    /// Read page `page` (0 .. BLOCKS × PAGES_PER_BLOCK) into `data` and,
    /// if given, its 64 spare bytes (4 × 16, the NFC's order).
    pub fn read_page(&self, page: u32, data: &mut [u8; PAGE], spare: Option<&mut [u8; SPARE]>) -> Result<Read, Error> {
        if page >= BLOCKS * PAGES_PER_BLOCK {
            return Err(Error::Range);
        }
        self.cmd(CMD_READ0)?;
        self.page_addr(page)?;
        self.cmd(CMD_READSTART)?;
        let mut r = Read::default();
        for i in 0..4 {
            self.w16(BUF_ADDR, i as u16);
            self.op(OP_OUTPUT)?;
            // Per subpage: bits 3:2 the data, 1:0 the spare (1: corrected,
            // 2: uncorrectable).
            let s = self.r16(ECC_STATUS);
            if s & 3 == 2 || (s >> 2) & 3 == 2 {
                return Err(Error::Ecc);
            }
            if s & 0xf != 0 {
                r.corrected += 1;
            }
        }
        self.copy_out(MAIN, data);
        if let Some(sp) = spare {
            self.copy_out(SPARE0, sp);
        }
        Ok(r)
    }

    /// Program page `page` with `data`; the spare bytes stay 0xff except
    /// where the NFC puts its ECC. The page must have been erased.
    pub fn program_page(&self, page: u32, data: &[u8; PAGE]) -> Result<(), Error> {
        if page >= BLOCKS * PAGES_PER_BLOCK {
            return Err(Error::Range);
        }
        self.copy_in(MAIN, data);
        for i in 0..SPARE / 4 {
            self.w32(SPARE0 + 4 * i, u32::MAX);
        }
        self.cmd(CMD_SEQIN)?;
        self.page_addr(page)?;
        for i in 0..4 {
            self.w16(BUF_ADDR, i as u16);
            self.op(OP_INPUT)?;
        }
        self.cmd(CMD_PAGEPROG)?;
        self.check_status()
    }

    /// Erase block `block`. Never call it for a bad block: its marker
    /// would be gone.
    pub fn erase_block(&self, block: u32) -> Result<(), Error> {
        if block >= BLOCKS {
            return Err(Error::Range);
        }
        self.cmd(CMD_ERASE1)?;
        self.row_addr(block * PAGES_PER_BLOCK)?;
        self.cmd(CMD_ERASE2)?;
        self.check_status()
    }

    /// Is `block` marked bad (by the factory, or by U-Boot or Linux)?
    pub fn is_bad(&self, block: u32) -> Result<bool, Error> {
        let mut data = [0u8; PAGE];
        let mut spare = [0u8; SPARE];
        for p in 0..2 {
            // An ECC error on a marked block is no surprise: the marker
            // is what counts (the spare is in the buffer either way).
            match self.read_page(block * PAGES_PER_BLOCK + p, &mut data, Some(&mut spare)) {
                Ok(_) => {}
                Err(Error::Ecc) => self.copy_out(SPARE0, &mut spare),
                Err(e) => return Err(e),
            }
            if spare[0] != 0xff {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The chip's status after a program or erase.
    fn check_status(&self) -> Result<(), Error> {
        self.cmd(CMD_STATUS)?;
        self.w16(BUF_ADDR, 0);
        self.op(OP_STATUS)?;
        if self.r32(MAIN) as u8 & STATUS_FAIL != 0 { Err(Error::Failed) } else { Ok(()) }
    }

    /// The buffer is read and written in 32-bit words (it doesn't take
    /// byte accesses).
    fn copy_out(&self, off: usize, out: &mut [u8]) {
        for (i, c) in out.chunks_mut(4).enumerate() {
            let w = self.r32(off + 4 * i).to_le_bytes();
            c.copy_from_slice(&w[..c.len()]);
        }
    }

    fn copy_in(&self, off: usize, data: &[u8]) {
        for (i, c) in data.chunks(4).enumerate() {
            let mut w = [0xffu8; 4];
            w[..c.len()].copy_from_slice(c);
            self.w32(off + 4 * i, u32::from_le_bytes(w));
        }
    }
}
