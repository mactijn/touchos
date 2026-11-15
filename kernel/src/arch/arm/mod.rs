//! 32-bit ARM: ARMv5TE, the ARM926EJ-S (i.MX25 in QEMU, i.MX27 in the Toon).
//! Provides the interface `arch` describes.
//!
//! No FPU, VIVT caches, no LDREX/STREX (atomics: `touchos-atomics`), no
//! execute-never bit.

mod atomics;
pub mod boot;
pub mod cache;
pub mod context;
pub mod paging;
pub mod syscall;
mod vectors;
pub use vectors::overflow_into_guard;

use core::arch::asm;

/// After `mem::init`: exception vectors (and the user atomic helpers).
pub fn init() {
    vectors::init();
}

/// No FPU registers to keep (software floating point).
pub struct FpuState;

impl FpuState {
    pub fn new() -> FpuState {
        FpuState
    }
    pub fn save(&mut self) {}
    pub fn restore(&self) {}
}

/// Names of the program counter and stack pointer, for fault reports.
pub const REG_NAMES: (&str, &str) = ("pc", "sp");

/// Interrupt control (the I and F bits of the CPSR).
pub mod irq {
    use core::arch::asm;

    /// Masks IRQs and FIQs; returns the old CPSR.
    #[inline(always)]
    fn mask() -> u32 {
        let cpsr: u32;
        unsafe { asm!("mrs {c}, cpsr", "orr {t}, {c}, #0xc0", "msr cpsr_c, {t}", c = out(reg) cpsr, t = out(reg) _, options(nostack)) };
        cpsr
    }

    pub fn disable() {
        mask();
    }
    pub fn enable() {
        unsafe { asm!("mrs {t}, cpsr", "bic {t}, {t}, #0xc0", "msr cpsr_c, {t}", t = out(reg) _, options(nostack)) };
    }
    pub fn are_enabled() -> bool {
        let cpsr: u32;
        unsafe { asm!("mrs {c}, cpsr", c = out(reg) cpsr, options(nomem, nostack, preserves_flags)) };
        cpsr & 0x80 == 0
    }
    pub fn without<R>(f: impl FnOnce() -> R) -> R {
        let old = mask();
        let r = f();
        unsafe { asm!("msr cpsr_c, {c}", c = in(reg) old, options(nostack)) };
        r
    }
    /// Wait for an interrupt (with interrupts masked: the ARM926 wakes up
    /// anyway), then let it be taken.
    pub fn enable_and_wait() {
        super::wait_for_interrupt();
        enable();
    }
}

/// ARM926 "wait for interrupt" (CP15 c7, c0, 4).
fn wait_for_interrupt() {
    unsafe { asm!("mcr p15, 0, {z}, c7, c0, 4", z = in(reg) 0u32, options(nostack)) };
}

/// The stack for traps from user mode is the thread's own SVC stack, which
/// the context switch keeps: nothing to do.
pub fn set_kernel_stack(_top: u64) {}

/// One core (the ARM926).
pub const MAX_CPUS: usize = 1;
pub fn cpu() -> usize {
    0
}
pub fn kick(_cpu: usize) {}
pub fn start_cpus(_max: usize) -> usize {
    1
}
pub fn halt_others() {}

pub fn halt_forever() -> ! {
    loop {
        irq::disable();
        wait_for_interrupt();
    }
}

/// The CPU's main ID register.
pub fn midr() -> u32 {
    let v: u32;
    unsafe { asm!("mrc p15, 0, {v}, c0, c0, 0", v = out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}
