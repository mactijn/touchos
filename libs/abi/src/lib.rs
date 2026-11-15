//! The contract between the touchos kernel and user processes; and, in
//! [`imx_boot`], between the i.MX boards' loader and the kernel.
//!
//! Every syscall argument and result is one register: a [`Word`] (32 bits
//! on ARM; 64 on a 64-bit host, where the tests run). Values that may need
//! more (times, deadlines) are passed as two words, low then high.
//!
//! A first result from -4095 to -1 is an [`Error`] ([`is_error`]); anything
//! else is a value (on 32-bit ARM, physical addresses above 2 GiB look
//! negative, so "negative" alone would not do).
//!
//! Syscall convention (`svc #0`, A32): `r7` = syscall number ([`nr`]);
//! arguments in `r0`-`r3`; returns `r0` (an error, see above) and `r1`.
//! Other registers are kept.

#![cfg_attr(not(test), no_std)]

use core::sync::atomic::{AtomicU32, AtomicU64};

pub mod imx_boot;

/// One syscall argument or result: a register.
pub type Word = usize;

/// Is this first syscall result an error code?
pub const fn is_error(r: Word) -> bool {
    r > Word::MAX - 4095
}

/// Splits a 64-bit value into the (low, high) words of two arguments.
pub const fn split64(v: u64) -> (Word, Word) {
    ((v & 0xffff_ffff) as Word, (v >> 32) as Word)
}

/// The inverse of [`split64`].
pub const fn join64(lo: Word, hi: Word) -> u64 {
    (lo as u64 & 0xffff_ffff) | (hi as u64) << 32
}

/// Syscall numbers. Groups are spaced out so they can grow.
pub mod nr {
    use super::Word;
    // Basic.
    pub const LOG: Word = 0;
    pub const EXIT: Word = 1;
    pub const YIELD: Word = 2;
    pub const WAIT: Word = 3;
    // System (need the System capability).
    pub const REBOOT: Word = 4;
    pub const TIME_SET: Word = 5;
    pub const KLOG_READ: Word = 6;
    pub const POWEROFF: Word = 7;
    // Capabilities.
    pub const CAP_MINT: Word = 8;
    pub const CAP_DROP: Word = 9;
    pub const CAP_INFO: Word = 10;
    // IPC.
    pub const CALL: Word = 12;
    pub const RECV: Word = 13;
    pub const REPLY_RECV: Word = 14;
    /// Set bits in a notification. Error::Dead: its thread has ended.
    pub const SIGNAL: Word = 15;
    pub const EP_CREATE: Word = 16;
    pub const NOTIF_CREATE: Word = 17;
    // Memory.
    pub const VM_ALLOC: Word = 20;
    // 21: reserved (VM_FREE, never implemented).
    pub const MEM_CREATE: Word = 22;
    pub const MEM_MAP: Word = 23;
    pub const MEM_UNMAP: Word = 24;
    pub const MEM_PHYS: Word = 25;
    pub const MMIO_MAP: Word = 26;
    pub const MMIO_DERIVE: Word = 27;
    // 30-34: reserved (were PCI_FIND, PCI_CFG_READ, PCI_CFG_WRITE,
    // PCI_BAR_MAP and IRQ_MSI).
    // Interrupts for drivers (kernel/src/irqs.rs).
    /// irq_root, line → an `Irq` capability for that interrupt
    /// controller line (Error::Full: someone holds it already).
    pub const IRQ_CLAIM: Word = 35;
    /// irq, bits: signal these notification bits of the calling thread
    /// when the line fires (the line is masked then), and unmask it.
    pub const IRQ_BIND: Word = 36;
    /// irq: the device's interrupt is handled; unmask the line.
    pub const IRQ_ACK: Word = 37;
    // Processes.
    pub const PROC_SPAWN: Word = 40;
    pub const PROC_KILL: Word = 41;
    pub const PROC_STATUS: Word = 42;
    // Threads (reserved).
    pub const THREAD_CREATE: Word = 44;
    // 46, 47: reserved (were EFI_VAR_GET/SET, the UEFI boot variables).
    // The hardware watchdog (System capability, right WATCHDOG): argument
    // [`watchdog`] op; returns 1 if a watchdog is running, else 0.
    pub const WATCHDOG: Word = 48;
    // Memory objects as wait queues (for rings in shared memory): MEM_WAIT
    // registers the caller to be signalled with these bits; MEM_NOTIFY
    // signals whoever is registered (the consumer of a ring, after a push).
    pub const MEM_WAIT: Word = 50;
    pub const MEM_NOTIFY: Word = 51;
    // Random bytes from the kernel's entropy source (for CPUs without an
    // instruction for it): buffer pointer, length (at most 256).
    pub const RANDOM: Word = 52;
}

/// Errors, returned as negative values in the first result register.
#[repr(isize)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Unknown syscall number.
    NoSys = -1,
    InvalidArgument = -2,
    /// The handle doesn't name a capability (of the right type).
    InvalidCap = -3,
    /// The capability lacks the rights for this operation.
    NoRights = -4,
    NoMemory = -5,
    /// A pointer argument isn't mapped (or not writable) in the caller.
    BadAddress = -6,
    /// A non-blocking operation would have had to wait.
    WouldBlock = -7,
    Timeout = -8,
    /// The other side (process, endpoint) is gone.
    Dead = -9,
    /// The capability table or another kernel table is full.
    Full = -10,
}

impl Error {
    pub fn from_raw(v: isize) -> Error {
        match v {
            -2 => Error::InvalidArgument,
            -3 => Error::InvalidCap,
            -4 => Error::NoRights,
            -5 => Error::NoMemory,
            -6 => Error::BadAddress,
            -7 => Error::WouldBlock,
            -8 => Error::Timeout,
            -9 => Error::Dead,
            -10 => Error::Full,
            _ => Error::NoSys,
        }
    }
}

/// Log levels for [`nr::LOG`] (same order as the `log` crate).
pub mod level {
    use super::Word;
    pub const ERROR: Word = 1;
    pub const WARN: Word = 2;
    pub const INFO: Word = 3;
    pub const DEBUG: Word = 4;
    pub const TRACE: Word = 5;
}

/// Fixed virtual addresses in every user address space.
pub mod layout {
    use super::Word;

    /// Below this nothing is mapped (null-pointer guard).
    pub const NULL_GUARD_END: Word = 0x20_0000;
    /// [`super::KInfo`], read-only, the same page in every process.
    pub const KINFO_VA: Word = 0x20_0000;
    /// [`super::BootInfo`], read-only, per process.
    pub const BOOTINFO_VA: Word = 0x20_1000;
    /// The clock's counter register page, read-only (boards whose clock
    /// counter user code can't read otherwise; see `KInfo::counter_va`).
    pub const COUNTER_VA: Word = 0x20_2000;
    /// IPC buffer of the (first) thread.
    pub const UTCB_VA: Word = 0x30_0000;
    /// Programs are linked here (libs/rt/user.ld).
    pub const IMAGE_BASE: Word = 0x40_0000;
    pub const DEFAULT_STACK_SIZE: Word = 256 * 1024;

    // A 64-bit host (the host tests and xtask): a 47-bit user half.
    #[cfg(not(target_arch = "arm"))]
    mod arch {
        use super::Word;
        pub const HEAP_BASE: Word = 0x1000_0000_0000;
        pub const SHARED_BASE: Word = 0x2000_0000_0000;
        pub const STACK_TOP: Word = 0x7fff_ff00_0000;
        pub const USER_END: Word = 0x0000_8000_0000_0000;
    }

    // ARM (32-bit): the lower 2 GiB; the kernel is at 0xc0000000. A
    // process's own memory (image, heap, stack) lies below 32 MiB, where
    // the ARM926's FCSE relocates it per process (the kernel's
    // arch/arm/paging.rs): the heap from 12 MiB to the stack, which ends at
    // 32 MiB. Device and DMA mappings from SHARED_BASE (per process);
    // cached shared memory in the kernel's global window from 1.5 GiB.
    #[cfg(target_arch = "arm")]
    mod arch {
        use super::Word;
        pub const HEAP_BASE: Word = 0x00c0_0000;
        pub const SHARED_BASE: Word = 0x4000_0000;
        pub const STACK_TOP: Word = 0x0200_0000;
        pub const USER_END: Word = 0x8000_0000;
    }

    /// The heap grows up from here (`VM_ALLOC`).
    pub const HEAP_BASE: Word = arch::HEAP_BASE;
    /// Shared memory, MMIO and DMA mappings are placed from here.
    pub const SHARED_BASE: Word = arch::SHARED_BASE;
    /// The main stack ends here (grows down), with a guard page below it.
    pub const STACK_TOP: Word = arch::STACK_TOP;
    /// First address that isn't user space; user pointers must be below.
    pub const USER_END: Word = arch::USER_END;
}

pub const KINFO_MAGIC: u64 = u64::from_le_bytes(*b"tchKINFO");
pub const BOOTINFO_MAGIC: u64 = u64::from_le_bytes(*b"tchBOOTI");
pub const ABI_VERSION: u32 = 2;

/// Kernel information shared read-only with every process, so reading the
/// clock needs no syscall.
///
/// The clock is a free-running counter (the SoC's GPT) and a
/// snapshot: at counter value `snap_count` the time since boot was
/// `snap_us`. Then `now_us = snap_us + (count - snap_count) * 1e6 /
/// counter_hz`; for a 32-bit counter `(count - snap_count)` wraps, and
/// `((delta * us_mult) >> us_shift)` avoids the division. The kernel renews
/// the snapshot (with `snap_seq` odd meanwhile) often enough that a 32-bit
/// delta never wraps twice.
#[repr(C)]
pub struct KInfo {
    pub magic: u64,
    pub version: u32,
    /// Odd while the kernel updates `snap_count`/`snap_us` (a seqlock).
    pub snap_seq: AtomicU32,
    pub counter_hz: u64,
    pub snap_count: AtomicU64,
    pub snap_us: AtomicU64,
    /// Where user code reads the counter: the user address of its 32-bit
    /// register.
    pub counter_va: u32,
    /// `us = (delta * us_mult) >> us_shift` for a 32-bit `delta`.
    pub us_mult: u32,
    pub us_shift: u32,
    pub _pad: u32,
    /// Unix time at boot (counter value 0 of the time since boot); updated
    /// by `TIME_SET` (NTP) or a real-time clock.
    pub boot_unix: AtomicU64,
    /// Timer ticks since boot.
    pub ticks: AtomicU64,
    /// The boot framebuffer (0 x 0 if there is none). Mapping it needs the
    /// "framebuffer" capability.
    pub fb: FbInfo,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct FbInfo {
    pub width: u32,
    pub height: u32,
    /// Bytes per row.
    pub pitch: u32,
    pub bpp: u16,
    pub r_shift: u8,
    pub g_shift: u8,
    pub b_shift: u8,
    pub _pad: [u8; 3],
}

/// One named capability handed to a process at start.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CapEntry {
    /// NUL-padded name, e.g. `"net"`, `"framebuffer"`.
    pub name: [u8; 16],
    pub slot: u32,
    pub kind: u32,
}

pub const MAX_BOOT_CAPS: usize = 48;

/// Per-process start information, read-only at [`layout::BOOTINFO_VA`].
/// `_start` receives its address in the first argument register.
#[repr(C)]
pub struct BootInfo {
    pub magic: u64,
    pub version: u32,
    pub ncaps: u32,
    /// NUL-padded process name.
    pub name: [u8; 16],
    pub cmdline_len: u32,
    pub _pad: u32,
    pub cmdline: [u8; 512],
    pub caps: [CapEntry; MAX_BOOT_CAPS],
}

/// Fixed capability slots in every process.
pub mod slot {
    pub const NULL: u32 = 0;
    /// The thread's own notification (signalled to wake its `WAIT`/`RECV`).
    pub const SELF_NOTIFICATION: u32 = 1;
    /// Endpoint of the supervisor (init); none in init itself.
    pub const SUPERVISOR: u32 = 2;
}

const _: () = assert!(core::mem::size_of::<BootInfo>() <= 4096);
const _: () = assert!(core::mem::size_of::<KInfo>() <= 4096);

/// Protection flags for `VM_ALLOC` / `MEM_MAP`. Pages are never writable
/// and executable at once.
pub mod prot {
    use super::Word;
    pub const READ: Word = 0;
    pub const WRITE: Word = 1 << 0;
    pub const EXEC: Word = 1 << 1;
}

/// Exit codes with a fixed meaning for the supervisor.
pub mod exit {
    use super::Word;
    pub const OK: Word = 0;
    /// "Start me again now": a deliberate restart (its settings changed),
    /// not a failure. init restarts a restartable program at once and does
    /// not count it, unless it asks for this again and again (then it is
    /// treated as failing).
    pub const RESTART: Word = 75;
    pub const PANIC: Word = 101;
}

/// Capability kinds (`CAP_INFO`, `CapEntry::kind`).
pub mod kind {
    pub const NONE: u32 = 0;
    pub const ENDPOINT: u32 = 1;
    pub const NOTIFICATION: u32 = 2;
    pub const MEMORY: u32 = 3;
    pub const MMIO: u32 = 4;
    pub const MMIO_ROOT: u32 = 5;
    // 6, 7: reserved (were PCI_DEVICE, PCI_ROOT).
    pub const PROCESS: u32 = 8;
    pub const SYSTEM: u32 = 9;
    pub const IRQ: u32 = 10;
    pub const IRQ_ROOT: u32 = 11;
}

/// Capability rights (meaning depends on the kind). `CAP_MINT` can only
/// remove rights.
pub mod rights {
    pub const ALL: u32 = u32::MAX;
    // Endpoint
    pub const CALL: u32 = 1 << 0;
    pub const RECV: u32 = 1 << 1;
    // Notification
    pub const SIGNAL: u32 = 1 << 0;
    // Memory
    pub const MAP_READ: u32 = 1 << 0;
    pub const MAP_WRITE: u32 = 1 << 1;
    /// Learn physical addresses (DMA drivers).
    pub const PHYS: u32 = 1 << 2;
    // Process
    pub const KILL: u32 = 1 << 0;
    pub const STATUS: u32 = 1 << 1;
    // System
    pub const REBOOT: u32 = 1 << 0;
    pub const TIME_SET: u32 = 1 << 1;
    pub const KLOG_READ: u32 = 1 << 2;
    // 1 << 3: reserved (was SPAWN, never checked: PROC_SPAWN needs only
    // read access to the program's memory object).
    pub const POWEROFF: u32 = 1 << 4;
    // 1 << 5: reserved (was EFI_BOOT).
    pub const WATCHDOG: u32 = 1 << 6;
}

/// Where REBOOT restarts into (its second argument).
pub mod reboot {
    use super::Word;
    /// The boot loader's choice (U-Boot boots the boot stage in NAND,
    /// which starts the active copy, or the copy on trial).
    pub const DEFAULT: Word = 0;
    // 1: reserved (was DEBIAN).
}

/// Operations of the WATCHDOG syscall.
pub mod watchdog {
    use super::Word;
    /// Reload the timer (a sign of life).
    pub const PET: Word = 0;
    /// Halt it until the next boot.
    pub const STOP: Word = 1;
    /// Only ask whether one is running.
    pub const QUERY: Word = 2;
}

/// Message tag (one register): label (16 bits), payload length in the UTCB
/// (12 bits), number of capabilities in `Utcb::caps` (4 bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tag(pub Word);

impl Tag {
    pub const fn new(label: u32, len: usize, ncaps: usize) -> Tag {
        Tag((label & 0xffff) as Word | (len & 0xfff) << 16 | (ncaps & 0xf) << 28)
    }
    pub const fn label(self) -> u32 {
        (self.0 & 0xffff) as u32
    }
    pub const fn len(self) -> usize {
        self.0 >> 16 & 0xfff
    }
    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }
    pub const fn ncaps(self) -> usize {
        self.0 >> 28 & 0xf
    }
}

/// `RECV` returned because notification bits were set (bits in the second
/// result, instead of the badge).
pub const LABEL_NOTIFIED: u32 = 0xffff;

pub const UTCB_DATA: usize = 4000;
pub const UTCB_CAPS: usize = 4;

/// The per-thread IPC buffer at `layout::UTCB_VA`. Before `CALL`/`REPLY`,
/// put the payload in `data` and capability slots in `caps`; after a
/// message arrives, `data` holds the payload and `caps` the slots the
/// received capabilities were given in our table.
#[repr(C)]
pub struct Utcb {
    pub caps: [u32; UTCB_CAPS],
    pub data: [u8; UTCB_DATA],
}

/// Arguments of `PROC_SPAWN` (in the caller's memory).
#[repr(C)]
pub struct SpawnArgs {
    pub name: [u8; 16],
    pub cmdline_len: u32,
    /// Scheduling priority, 2 (first) ..= 200.
    pub priority: u32,
    pub cmdline: [u8; 512],
    /// Notification signalled with `death_bits` when the process ends
    /// (slot in the caller's table; 0 = none).
    pub death_notification: u32,
    pub ncaps: u32,
    pub death_bits: Word,
    /// Capabilities to give the child: name + slot in the caller's table.
    pub caps: [CapEntry; MAX_BOOT_CAPS],
}

/// `PROC_STATUS` results (first result; the exit code or fault vector in the
/// second).
pub mod status {
    use super::Word;
    pub const RUNNING: Word = 0;
    pub const EXITED: Word = 1;
    /// Killed by a CPU exception (second result: the vector) or `PROC_KILL`
    /// (second result: [`KILLED_BY_REQUEST`]).
    pub const KILLED: Word = 2;
    pub const KILLED_BY_REQUEST: Word = 0xffff;
}

/// Memory flags for `MEM_CREATE`.
pub mod mem {
    use super::Word;
    /// Physically contiguous (and below 4 GiB): for DMA. Never freed.
    pub const CONTIGUOUS: Word = 1 << 0;
}

const _: () = assert!(core::mem::size_of::<Utcb>() <= 4096);
const _: () = assert!(core::mem::size_of::<SpawnArgs>() <= 4096);
const _: () = assert!(UTCB_DATA < 1 << 12 && UTCB_CAPS < 1 << 4);
const _: () = assert!(core::mem::align_of::<KInfo>() == 8);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_the_top_4095_words() {
        assert!(!is_error(0));
        assert!(!is_error(Word::MAX - 4095)); // a value: e.g. a high physical address
        assert!(is_error(Word::MAX - 4094)); // -4095
        assert!(is_error(Word::MAX)); // -1
        for e in [
            Error::NoSys,
            Error::InvalidArgument,
            Error::InvalidCap,
            Error::NoRights,
            Error::NoMemory,
            Error::BadAddress,
            Error::WouldBlock,
            Error::Timeout,
            Error::Dead,
            Error::Full,
        ] {
            let raw = e as isize;
            assert!(is_error(raw as Word), "{e:?}");
            assert_eq!(Error::from_raw(raw), e);
        }
        assert_eq!(Error::from_raw(-4000), Error::NoSys); // unknown: as NoSys
    }

    #[test]
    fn two_word_values() {
        for v in [0, 1, 0xffff_ffff, 0x1_0000_0000, 0x1234_5678_9abc_def0, u64::MAX] {
            let (lo, hi) = split64(v);
            assert_eq!(join64(lo, hi), v);
            assert!(lo <= 0xffff_ffff && hi <= 0xffff_ffff);
        }
        // Junk above the low 32 bits of `lo` (a 64-bit register) is ignored.
        assert_eq!(join64(0xdead_0000_0000_0001, 2), 0x2_0000_0001);
    }

    #[test]
    fn tags() {
        let t = Tag::new(0x1234, UTCB_DATA, UTCB_CAPS);
        assert_eq!((t.label(), t.len(), t.ncaps(), t.is_empty()), (0x1234, UTCB_DATA, UTCB_CAPS, false));
        assert!(Tag::new(7, 0, 0).is_empty());
        // Every field at its widest, and too wide: cut to the field, never
        // spilling into the next one.
        let max = Tag::new(0xffff, 0xfff, 0xf);
        assert_eq!((max.label(), max.len(), max.ncaps()), (0xffff, 0xfff, 0xf));
        let over = Tag::new(0x1_0001, 0x1001, 0x11);
        assert_eq!((over.label(), over.len(), over.ncaps()), (1, 1, 1));
        assert_eq!(Tag::new(LABEL_NOTIFIED, 0, 0).label(), LABEL_NOTIFIED);
    }
}

