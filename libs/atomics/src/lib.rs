//! Atomic operations for ARMv5TE.
//!
//! ARMv5 has no LDREX/STREX, so LLVM turns every atomic read-modify-write
//! into a library call (targets/armv5te-touchos.json enables atomics):
//! `__sync_*_{1,2,4}` for up to 32 bits, `__atomic_*_8` for 64 bits and
//! `__sync_synchronize` for fences. Plain 32-bit loads and stores stay inline.
//! compiler-builtins only provides these for ARM Linux, so this crate does.
//!
//! Everything is built on a 32- and a 64-bit compare-and-swap (`generic`),
//! from one of two backends, chosen with a feature:
//! - `kernel`: with IRQs and FIQs masked (there is one CPU);
//! - `user`:   through the kernel's restartable helpers in the vectors page
//!   (see [`helper`]); user mode cannot mask interrupts.
//!
//! 64-bit loads and stores are a single LDRD/STRD, which an interrupt cannot
//! split on one CPU.
//!
//! Link this crate into every ARM binary (the symbols are found by the
//! linker; nothing needs to be called).
#![cfg_attr(not(test), no_std)]

pub mod generic;

/// Fixed addresses of the user helpers in the kernel's vectors page
/// (0xffff0000). The kernel rewinds a user thread interrupted inside one of
/// them to its start, so each runs as if atomic (kernel/src/arch/arm/
/// atomics.rs). Calling convention (AAPCS):
/// - `CAS32`: `fn(p: *mut u32, old: u32, new: u32) -> u32` (the value found);
/// - `CAS64`: `fn(p: *mut u64, args: *const [u32; 4]) -> u64`, where args is
///   old (lo, hi) then new (lo, hi), 8-byte aligned; returns the value found.
pub mod helper {
    pub const CAS32: usize = 0xffff_0f80;
    pub const CAS64: usize = 0xffff_0fa0;
}

#[cfg(all(target_arch = "arm", not(test)))]
mod libcalls;
