//! Syscalls (see `touchos_abi` for the convention). Thin, typed wrappers;
//! capability slots are plain `u32`s.

use core::arch::asm;
use touchos_abi::{Error, SpawnArgs, Tag, Word, is_error, nr, split64};

pub type Result<T> = core::result::Result<T, Error>;

/// `svc #0`, number in r7, arguments in r0-r3, results in r0/r1; the
/// kernel keeps every other register.
#[inline(always)]
unsafe fn syscall(n: Word, a: Word, b: Word, c: Word, d: Word) -> (isize, Word) {
    let ret: isize;
    let ret2: Word;
    unsafe {
        asm!(
            "svc #0",
            in("r7") n,
            inlateout("r0") a as isize => ret,
            inlateout("r1") b => ret2,
            in("r2") c,
            in("r3") d,
            options(nostack),
        );
    }
    (ret, ret2)
}

/// Arguments are converted to words here: values that don't fit a word are
/// passed as two (see `split64`).
fn sc(n: Word, a: u64, b: u64, c: u64, d: u64) -> Result<(u64, u64)> {
    let (r, r2) = unsafe { syscall(n, a as Word, b as Word, c as Word, d as Word) };
    if is_error(r as Word) {
        Err(Error::from_raw(r))
    } else {
        Ok((r as Word as u64, r2 as u64))
    }
}

fn sc1(n: Word, a: u64, b: u64, c: u64, d: u64) -> Result<u64> {
    sc(n, a, b, c, d).map(|r| r.0)
}

/// A 64-bit value as two word arguments.
fn halves(v: u64) -> (u64, u64) {
    let (lo, hi) = split64(v);
    (lo as u64, hi as u64)
}

/// Write one line to the kernel log (serial, screen console, UDP log).
pub fn log(level: Word, msg: &str) {
    let _ = sc(nr::LOG, msg.as_ptr() as u64, msg.len() as u64, level as u64, 0);
}

/// End this process. The supervisor sees `code`.
pub fn exit(code: Word) -> ! {
    let _ = sc(nr::EXIT, code as u64, 0, 0, 0);
    loop {
        yield_now();
    }
}

/// Give the CPU to another runnable thread.
pub fn yield_now() {
    let _ = sc(nr::YIELD, 0, 0, 0, 0);
}

/// Block until our notification is signalled or `deadline_us` (microseconds
/// since boot; 0 = don't block, `u64::MAX` = forever). Returns the
/// notification bits that were set (and clears them).
pub fn wait(deadline_us: u64) -> Result<usize> {
    let (lo, hi) = halves(deadline_us);
    sc1(nr::WAIT, lo, hi, 0, 0).map(|b| b as usize)
}

pub fn reboot(system: u32) -> Result<()> {
    sc1(nr::REBOOT, system as u64, touchos_abi::reboot::DEFAULT as u64, 0, 0).map(|_| ())
}

/// Switch the machine off. Returns if it can't (the Toon has no power
/// switch).
pub fn poweroff(system: u32) -> Result<()> {
    sc1(nr::POWEROFF, system as u64, 0, 0, 0).map(|_| ())
}

pub fn time_set(system: u32, unix: u64) -> Result<()> {
    let (lo, hi) = halves(unix);
    sc1(nr::TIME_SET, system as u64, lo, hi, 0).map(|_| ())
}

/// Kernel log lines from `cursor` on; returns (bytes, next cursor).
pub fn klog_read(system: u32, buf: &mut [u8], cursor: u64) -> Result<(usize, u64)> {
    sc(
        nr::KLOG_READ,
        system as u64,
        buf.as_mut_ptr() as u64,
        buf.len() as u64,
        cursor,
    )
    .map(|(n, c)| (n as usize, c))
}

/// The kernel's figures (`touchos_abi::stats`): fills `buf` → (bytes
/// written, bytes needed); 0 written if `buf` is too small.
pub fn stats_raw(system: u32, buf: &mut [u8]) -> Result<(usize, usize)> {
    sc(nr::STATS, system as u64, buf.as_mut_ptr() as u64, buf.len() as u64, 0).map(|(n, m)| (n as usize, m as usize))
}

/// The kernel's figures as a buffer holding a whole report (read it with
/// `touchos_abi::stats::Report::parse`); grows the buffer as threads come.
pub fn stats(system: u32) -> Result<alloc::vec::Vec<u8>> {
    let mut buf = alloc::vec![0u8; 2048];
    loop {
        let (n, need) = stats_raw(system, &mut buf)?;
        if n > 0 {
            buf.truncate(n);
            return Ok(buf);
        }
        // Room for a few more threads that may start meanwhile.
        buf.resize(need + 256, 0);
    }
}

/// The hardware watchdog: `touchos_abi::watchdog::PET`, `STOP` or `QUERY`.
/// Returns whether one is running.
pub fn watchdog(system: u32, op: Word) -> Result<bool> {
    sc1(nr::WATCHDOG, system as u64, op as u64, 0, 0).map(|v| v != 0)
}

/// A copy of a capability with fewer rights and/or a badge.
pub fn cap_mint(slot: u32, rights: u32, badge: usize) -> Result<u32> {
    sc1(nr::CAP_MINT, slot as u64, rights as u64, badge as u64, 0).map(|s| s as u32)
}

pub fn cap_drop(slot: u32) -> Result<()> {
    sc1(nr::CAP_DROP, slot as u64, 0, 0, 0).map(|_| ())
}

/// (kind, rights (the low 16 bits), size in bytes for memory and MMIO) of a
/// capability.
pub fn cap_info(slot: u32) -> Result<(u32, u32, u64)> {
    sc(nr::CAP_INFO, slot as u64, 0, 0, 0).map(|(k, s)| ((k & 0xff) as u32, (k >> 8) as u32, s))
}

/// Send the message in our UTCB and wait for the reply (also in the UTCB).
pub fn call(ep: u32, tag: Tag) -> Result<Tag> {
    sc1(nr::CALL, ep as u64, tag.0 as u64, 0, 0).map(|t| Tag(t as Word))
}

/// Wait for a message (returns (tag, badge)) or notification (tag label
/// `LABEL_NOTIFIED`, bits instead of the badge), until `deadline_us`.
pub fn recv(ep: u32, deadline_us: u64) -> Result<(Tag, usize)> {
    let (lo, hi) = halves(deadline_us);
    sc(nr::RECV, ep as u64, lo, hi, 0).map(|(t, b)| (Tag(t as Word), b as usize))
}

/// Reply to the last caller with the message in our UTCB; then, unless
/// `ep` is 0, receive like [`recv`].
pub fn reply_recv(ep: u32, tag: Tag, deadline_us: u64) -> Result<(Tag, usize)> {
    let (lo, hi) = halves(deadline_us);
    sc(nr::REPLY_RECV, ep as u64, tag.0 as u64, lo, hi).map(|(t, b)| (Tag(t as Word), b as usize))
}

pub fn signal(notification: u32, bits: usize) -> Result<()> {
    sc1(nr::SIGNAL, notification as u64, bits as u64, 0, 0).map(|_| ())
}

pub fn ep_create() -> Result<u32> {
    sc1(nr::EP_CREATE, 0, 0, 0, 0).map(|s| s as u32)
}

/// A capability for our own notification, to give to others.
pub fn notif_create() -> Result<u32> {
    sc1(nr::NOTIF_CREATE, 0, 0, 0, 0).map(|s| s as u32)
}

/// Map `len` bytes of fresh zeroed memory at `va` (page aligned, unmapped).
pub fn vm_alloc(va: u64, len: u64, prot: Word) -> Result<u64> {
    sc1(nr::VM_ALLOC, va, len, prot as u64, 0)
}

pub fn mem_create(len: u64, flags: Word) -> Result<u32> {
    sc1(nr::MEM_CREATE, len, flags as u64, 0, 0).map(|s| s as u32)
}

/// Map `len` bytes of a memory object from byte `off` (page aligned).
pub fn mem_map(mem: u32, off: u64, len: u64, prot: Word) -> Result<u64> {
    sc1(nr::MEM_MAP, mem as u64, off, len, prot as u64)
}

pub fn mem_unmap(va: u64) -> Result<()> {
    sc1(nr::MEM_UNMAP, va, 0, 0, 0).map(|_| ())
}

pub fn mem_phys(mem: u32, off: u64) -> Result<u64> {
    sc1(nr::MEM_PHYS, mem as u64, off, 0, 0)
}

pub fn mmio_derive(root: u32, phys: u64, len: u64) -> Result<u32> {
    sc1(nr::MMIO_DERIVE, root as u64, phys, len, 0).map(|s| s as u32)
}

pub fn mmio_map(mmio: u32, off: u64, len: u64) -> Result<u64> {
    sc1(nr::MMIO_MAP, mmio as u64, off, len, 0)
}

/// Claim interrupt line `line` (an `Irq` capability; init does this for
/// the drivers). NoSys where drivers poll (the PC).
pub fn irq_claim(root: u32, line: u32) -> Result<u32> {
    sc1(nr::IRQ_CLAIM, root as u64, line as u64, 0, 0).map(|s| s as u32)
}

/// Signal `bits` (of this thread) when the line fires; it is masked then
/// until [`irq_ack`].
pub fn irq_bind(irq: u32, bits: usize) -> Result<()> {
    sc1(nr::IRQ_BIND, irq as u64, bits as u64, 0, 0).map(|_| ())
}

/// The device's interrupt is handled (its status cleared): unmask.
pub fn irq_ack(irq: u32) -> Result<()> {
    sc1(nr::IRQ_ACK, irq as u64, 0, 0, 0).map(|_| ())
}

/// Start a program stored in a memory object; returns (process cap, pid).
pub fn proc_spawn(mem: u32, off: u64, len: u64, args: &SpawnArgs) -> Result<(u32, u32)> {
    sc(
        nr::PROC_SPAWN,
        mem as u64,
        off,
        len,
        args as *const SpawnArgs as u64,
    )
    .map(|(s, p)| (s as u32, p as u32))
}

pub fn proc_kill(process: u32) -> Result<()> {
    sc1(nr::PROC_KILL, process as u64, 0, 0, 0).map(|_| ())
}

/// (`touchos_abi::status` value, exit code or fault vector).
pub fn proc_status(process: u32) -> Result<(Word, Word)> {
    sc(nr::PROC_STATUS, process as u64, 0, 0, 0).map(|(s, c)| (s as Word, c as Word))
}

/// Be signalled with `bits` when someone calls [`mem_notify`] on `mem`.
pub fn mem_wait(mem: u32, bits: usize) -> Result<()> {
    sc1(nr::MEM_WAIT, mem as u64, bits as u64, 0, 0).map(|_| ())
}

/// Fill `buf` (at most 256 bytes) from the kernel's entropy source.
pub fn random(buf: &mut [u8]) -> Result<()> {
    sc1(nr::RANDOM, buf.as_mut_ptr() as u64, buf.len() as u64, 0, 0).map(|_| ())
}

/// Signal whoever waits on `mem` (see [`mem_wait`]).
pub fn mem_notify(mem: u32) -> Result<()> {
    sc1(nr::MEM_NOTIFY, mem as u64, 0, 0, 0).map(|_| ())
}
