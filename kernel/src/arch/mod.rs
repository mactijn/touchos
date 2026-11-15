//! The CPU architecture (32-bit ARM, ARMv5TE): traps, context switch,
//! paging, interrupt control, behind module names generic code relies on.
//!
//! What generic code uses from every architecture:
//! - `irq`: interrupt control (`disable`, `without`, `enable_and_wait`, ...);
//! - `paging`: page tables (`PageTable`), kernel and MMIO mappings, the
//!   cache maintenance a VIVT cache needs (`sync_user`, `sync_direct`,
//!   `sync_all`), `CACHE_ALIASES`, `DMA_COHERENT`,
//!   `KSTACK_WINDOW`, `kernel_root`, `switch_to`; the page attributes are
//!   generic (`mem::Attr`, `mem::MemKind`);
//! - `context`: new kernel and user stacks, and the context switch;
//! - `syscall::SyscallFrame`: syscall number, arguments and results;
//! - `FpuState`, `set_kernel_stack`, `halt_forever`, `REG_NAMES`.
//!
//! What an architecture calls in generic code: `sys::dispatch` (a
//! syscall), `task::user_fault` (a CPU exception in user code),
//! `task::thread_start` (a new user thread's first code). The interrupt
//! controller and the timer are the board's (the SoC's AITC and GPT): the
//! IRQ vector calls `board::irq::dispatch`, and the board calls
//! `time::tick`.

/// Address-space switches so far (wraps; the heartbeat logs the rate). On
/// the ARM926 each one costs a full cache flush, so this is worth watching.
pub static SWITCHES: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

mod arm;
pub use self::arm::*;
