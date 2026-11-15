//! The i.MX27's SAHARA2 crypto engine, used only for its hardware random
//! number generator: the Toon's one real source of randomness (its clocks
//! all come from one crystal, so timer jitter is weak there).
//!
//! Polled, one descriptor at a time, as Freescale's BSP driver does at
//! reset on the i.MX27 (drivers/mxc/security/sahara2: sah_HW_Reset,
//! fsl_shw_get_random). Mainline Linux drives the same block for AES and
//! SHA (drivers/crypto/sahara.c) but not for random numbers.
//!
//! A descriptor is 6 words { header, len1, ptr1, len2, ptr2, next } in
//! memory the engine reads by DMA; the RNG "generate" header with the LLO
//! bit (ptr1 points straight at the output) is 0xB18C0000.

use spin::Mutex;

use crate::arch::paging::sync_direct;
use crate::board::imx::ccm;
use crate::mem;

const VERSION: usize = 0x00;
const DAR: usize = 0x04;
const CONTROL: usize = 0x08;
const CMD: usize = 0x0c;
const STATUS: usize = 0x10;
const ERRSTATUS: usize = 0x14;
const FADDR: usize = 0x18;
/// The descriptor the engine fetched last.
const CDAR: usize = 0x1c;

const CMD_RESET: u32 = 1 << 0;
const CMD_CLEAR: u32 = 1 << 8 | 1 << 9; // interrupt and error
const CMD_BATCH: u32 = 1 << 16;
/// Control: 8-word DMA bursts, interrupts off.
const CONTROL_BURST8: u32 = 8 << 16;
const CONTROL_RNG_AUTORESEED: u32 = 1 << 7;
/// STATUS bits 2:0.
const STATE_IDLE: u32 = 0;
const STATE_BUSY: u32 = 1;
const STATE_DONE: u32 = 4;
const STATE_DONE1_BUSY2: u32 = 5;

const HDR_RNG_GENERATE_LLO: u32 = 0xb18c_0000;
/// Bytes per descriptor: a word multiple, well within one 1 KiB block.
const CHUNK: usize = 64;
/// The output buffer's offset in the page; the two descriptors are at 0
/// and 32 (used in turn, so CDAR shows which one the engine took).
const OUT: usize = 64;

struct Sahara {
    regs: usize,
    /// One page: the descriptors, then the output.
    page: u64,
    /// Which descriptor slot the next run uses.
    slot: usize,
}

static SAHARA: Mutex<Option<Sahara>> = Mutex::new(None);

impl Sahara {
    fn r(&self, off: usize) -> u32 {
        unsafe { ((self.regs + off) as *const u32).read_volatile() }
    }
    fn w(&self, off: usize, v: u32) {
        unsafe { ((self.regs + off) as *mut u32).write_volatile(v) }
    }
    fn state(&self) -> u32 {
        self.r(STATUS) & 7
    }

    /// Wait until a reset has finished (the state is no longer busy).
    fn wait_reset(&self) -> u32 {
        for _ in 0..1_000_000 {
            let s = self.state();
            if s != STATE_BUSY && s != STATE_DONE1_BUSY2 {
                return s;
            }
        }
        STATE_BUSY
    }

    /// Wait until the descriptor at `desc` has run. The state alone doesn't
    /// tell: only the first descriptor after a reset ends in DONE; later
    /// ones go straight back to IDLE (seen on the Toon). So: the engine
    /// has fetched this descriptor (CDAR) and is no longer busy, or it
    /// stopped with an error or fault. Returns the state.
    fn wait_done(&self, desc: u32) -> u32 {
        for _ in 0..1_000_000 {
            let s = self.state();
            let fetched = self.r(CDAR) == desc;
            match s {
                STATE_IDLE | STATE_DONE if fetched => return STATE_DONE,
                STATE_IDLE | STATE_BUSY | STATE_DONE1_BUSY2 | STATE_DONE => {}
                _ => return s, // error, fault
            }
        }
        self.state()
    }

    /// Generate `n` (a multiple of 4, at most CHUNK) random bytes into `out`.
    fn generate(&mut self, out: &mut [u8]) -> Result<(), &'static str> {
        let n = out.len();
        self.slot ^= 1;
        let desc_phys = self.page + 32 * self.slot as u64;
        let desc = mem::phys_to_virt(desc_phys) as *mut u32;
        let words = [HDR_RNG_GENERATE_LLO, n as u32, (self.page + OUT as u64) as u32, 0, 0, 0];
        unsafe {
            for (i, &w) in words.iter().enumerate() {
                desc.add(i).write_volatile(w);
            }
            core::ptr::write_bytes(mem::phys_to_virt(self.page + OUT as u64), 0, n);
        }
        // The descriptor to RAM, and no dirty line over the output (the
        // engine writes it behind the cache's back).
        sync_direct(self.page, OUT + n);
        if !matches!(self.state(), STATE_IDLE | STATE_DONE) {
            return Err("not idle");
        }
        self.w(DAR, desc_phys as u32);
        let state = self.wait_done(desc_phys as u32);
        if state != STATE_DONE {
            let (faddr, err) = (self.r(FADDR), self.r(ERRSTATUS)); // reading ERRSTATUS clears it
            sync_direct(self.page + OUT as u64, 4);
            let out0 = unsafe { (mem::phys_to_virt(self.page + OUT as u64) as *const u32).read_volatile() };
            self.w(CMD, CMD_BATCH | CMD_CLEAR);
            ::log::warn!(
                "sahara: state {state} (status {:#010x}), error status {err:#010x}, fault address {faddr:#010x}, output {out0:#010x}",
                self.r(STATUS)
            );
            return Err("generate failed");
        }
        // Acknowledge (interrupt and error status), staying in batch mode.
        self.w(CMD, CMD_BATCH | CMD_CLEAR);
        // Drop any stale lines before reading what the engine wrote.
        sync_direct(self.page + OUT as u64, n);
        unsafe { core::ptr::copy_nonoverlapping(mem::phys_to_virt(self.page + OUT as u64), out.as_mut_ptr(), n) };
        if out.iter().all(|&b| b == 0) {
            return Err("all zeros");
        }
        Ok(())
    }
}

/// Clock it, reset it, and start the RNG. Returns whether it works.
pub fn init(base: u64) -> bool {
    // The stock kernel leaves its clocks off when idle.
    ccm::ungate(&[ccm::SAHARA_IPG, ccm::SAHARA_AHB]);
    let mut s = Sahara { regs: mem::map_mmio(base, 0x100), page: mem::alloc_frame(), slot: 0 };
    let version = s.r(VERSION);
    if version != 3 {
        ::log::warn!("sahara: version {version}, expected 3; not used");
        return false;
    }
    s.w(CMD, CMD_BATCH | CMD_RESET);
    if s.wait_reset() != STATE_IDLE {
        ::log::warn!("sahara: not idle after reset (status {:#010x}); not used", s.r(STATUS));
        return false;
    }
    s.w(CONTROL, CONTROL_BURST8);
    // A hardware bug: the RNG must generate once before auto-reseeding may
    // be switched on (the BSP's workaround, active on the i.MX27).
    let mut first = [0u8; 12];
    if let Err(e) = s.generate(&mut first) {
        ::log::warn!("sahara: first random number: {e}; not used");
        return false;
    }
    s.w(CONTROL, CONTROL_BURST8 | CONTROL_RNG_AUTORESEED);
    *SAHARA.lock() = Some(s);
    true
}

/// Fill `out` with hardware random bytes. False if there is no SAHARA or
/// it failed (then `out` is unchanged where it failed).
pub fn fill(out: &mut [u8]) -> bool {
    let mut g = SAHARA.lock();
    let Some(s) = g.as_mut() else { return false };
    let mut buf = [0u8; CHUNK];
    for chunk in out.chunks_mut(CHUNK) {
        let n = chunk.len().next_multiple_of(4);
        if s.generate(&mut buf[..n]).is_err() {
            return false;
        }
        chunk.copy_from_slice(&buf[..chunk.len()]);
    }
    true
}
