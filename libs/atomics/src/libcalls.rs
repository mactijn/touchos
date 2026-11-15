//! The exported libcalls (ARM only), on the chosen backend.

use core::arch::asm;

use crate::generic::{Prim, cas_small, rmw32, rmw64, rmw_small, sext};

/// One LDRD: atomic with respect to interrupts on one CPU. `p` must be
/// 8-byte aligned (as `AtomicU64` is).
#[inline(always)]
unsafe fn ldrd(p: *const u64) -> u64 {
    let (lo, hi): (u32, u32);
    unsafe { asm!("ldrd r2, r3, [{p}]", p = in(reg) p, out("r2") lo, out("r3") hi, options(nostack, readonly, preserves_flags)) };
    (hi as u64) << 32 | lo as u64
}

#[inline(always)]
unsafe fn strd(p: *mut u64, v: u64) {
    unsafe {
        asm!("strd r2, r3, [{p}]", p = in(reg) p, in("r2") v as u32, in("r3") (v >> 32) as u32,
             options(nostack, preserves_flags))
    };
}

#[cfg(feature = "kernel")]
mod backend {
    use super::*;
    use core::ptr::{read_volatile, write_volatile};

    /// Masks IRQs and FIQs; returns the old CPSR.
    #[inline(always)]
    fn mask() -> u32 {
        let cpsr: u32;
        unsafe { asm!("mrs {c}, cpsr", "orr {t}, {c}, #0xc0", "msr cpsr_c, {t}", c = out(reg) cpsr, t = out(reg) _, options(nostack)) };
        cpsr
    }

    #[inline(always)]
    fn restore(cpsr: u32) {
        unsafe { asm!("msr cpsr_c, {c}", c = in(reg) cpsr, options(nostack)) };
    }

    pub struct Backend;
    impl Prim for Backend {
        unsafe fn cas32(p: *mut u32, old: u32, new: u32) -> u32 {
            let s = mask();
            let cur = unsafe { read_volatile(p) };
            if cur == old {
                unsafe { write_volatile(p, new) };
            }
            restore(s);
            cur
        }
        unsafe fn cas64(p: *mut u64, old: u64, new: u64) -> u64 {
            let s = mask();
            let cur = unsafe { ldrd(p) };
            if cur == old {
                unsafe { strd(p, new) };
            }
            restore(s);
            cur
        }
        unsafe fn load64(p: *const u64) -> u64 {
            unsafe { ldrd(p) }
        }
        unsafe fn store64(p: *mut u64, v: u64) {
            unsafe { strd(p, v) }
        }
    }

    /// Orders memory for other threads and for devices: drain the write
    /// buffer (CP15 c7, c10, 4).
    pub fn barrier() {
        unsafe { asm!("mcr p15, 0, {z}, c7, c10, 4", z = in(reg) 0u32, options(nostack, preserves_flags)) };
    }
}

#[cfg(all(feature = "user", not(feature = "kernel")))]
mod backend {
    use super::*;
    use crate::helper;

    pub struct Backend;
    impl Prim for Backend {
        unsafe fn cas32(p: *mut u32, old: u32, new: u32) -> u32 {
            let f: extern "C" fn(*mut u32, u32, u32) -> u32 = unsafe { core::mem::transmute(helper::CAS32) };
            f(p, old, new)
        }
        unsafe fn cas64(p: *mut u64, old: u64, new: u64) -> u64 {
            #[repr(C, align(8))]
            struct Args([u32; 4]);
            let args = Args([old as u32, (old >> 32) as u32, new as u32, (new >> 32) as u32]);
            let f: extern "C" fn(*mut u64, *const Args) -> u64 = unsafe { core::mem::transmute(helper::CAS64) };
            f(p, &args)
        }
        unsafe fn load64(p: *const u64) -> u64 {
            unsafe { ldrd(p) }
        }
        unsafe fn store64(p: *mut u64, v: u64) {
            unsafe { strd(p, v) }
        }
    }

    /// User mode cannot drain the write buffer (CP15 is privileged). Memory
    /// shared with devices is mapped non-cacheable, non-bufferable instead;
    /// between threads on one CPU, program order is enough.
    pub fn barrier() {
        // An empty asm block is a compiler barrier (it may touch memory).
        // Not compiler_fence: on ARMv5 LLVM lowers that to a call of
        // __sync_synchronize, which is this function.
        unsafe { asm!("", options(nostack, preserves_flags)) };
    }
}

#[cfg(not(any(feature = "kernel", feature = "user")))]
compile_error!("touchos-atomics on ARM needs the `kernel` or the `user` feature");

use backend::{Backend as B, barrier};

#[unsafe(no_mangle)]
pub extern "C" fn __sync_synchronize() {
    barrier();
}

/// The 8/16/32-bit family. Values travel as 32-bit words (masked here), so
/// nothing depends on how the caller extended them.
macro_rules! sync_family {
    ($size:literal, $n:literal) => {
        const _: () = {
            #[inline(always)]
            unsafe fn update(p: *mut u8, f: impl Fn(u32) -> u32) -> u32 {
                if $size == 4 {
                    unsafe { rmw32::<B>(p as *mut u32, f) }
                } else {
                    unsafe { rmw_small::<B>(p, $size, f) }
                }
            }
            #[inline(always)]
            unsafe fn cas(p: *mut u8, old: u32, new: u32) -> u32 {
                if $size == 4 {
                    unsafe { B::cas32(p as *mut u32, old, new) }
                } else {
                    unsafe { cas_small::<B>(p, $size, old, new) }
                }
            }

            #[unsafe(export_name = concat!("__sync_val_compare_and_swap_", $n))]
            pub unsafe extern "C" fn val_cas(p: *mut u8, old: u32, new: u32) -> u32 {
                unsafe { cas(p, old, new) }
            }
            #[unsafe(export_name = concat!("__sync_bool_compare_and_swap_", $n))]
            pub unsafe extern "C" fn bool_cas(p: *mut u8, old: u32, new: u32) -> bool {
                let m = if $size == 4 { u32::MAX } else { (1u32 << ($size * 8)) - 1 };
                unsafe { cas(p, old, new) == old & m }
            }
            #[unsafe(export_name = concat!("__sync_lock_test_and_set_", $n))]
            pub unsafe extern "C" fn swap(p: *mut u8, v: u32) -> u32 {
                unsafe { update(p, |_| v) }
            }
            #[unsafe(export_name = concat!("__sync_fetch_and_add_", $n))]
            pub unsafe extern "C" fn add(p: *mut u8, v: u32) -> u32 {
                unsafe { update(p, |x| x.wrapping_add(v)) }
            }
            #[unsafe(export_name = concat!("__sync_fetch_and_sub_", $n))]
            pub unsafe extern "C" fn sub(p: *mut u8, v: u32) -> u32 {
                unsafe { update(p, |x| x.wrapping_sub(v)) }
            }
            #[unsafe(export_name = concat!("__sync_fetch_and_and_", $n))]
            pub unsafe extern "C" fn and(p: *mut u8, v: u32) -> u32 {
                unsafe { update(p, |x| x & v) }
            }
            #[unsafe(export_name = concat!("__sync_fetch_and_or_", $n))]
            pub unsafe extern "C" fn or(p: *mut u8, v: u32) -> u32 {
                unsafe { update(p, |x| x | v) }
            }
            #[unsafe(export_name = concat!("__sync_fetch_and_xor_", $n))]
            pub unsafe extern "C" fn xor(p: *mut u8, v: u32) -> u32 {
                unsafe { update(p, |x| x ^ v) }
            }
            #[unsafe(export_name = concat!("__sync_fetch_and_nand_", $n))]
            pub unsafe extern "C" fn nand(p: *mut u8, v: u32) -> u32 {
                unsafe { update(p, |x| !(x & v)) }
            }
            #[unsafe(export_name = concat!("__sync_fetch_and_max_", $n))]
            pub unsafe extern "C" fn max(p: *mut u8, v: u32) -> u32 {
                unsafe { update(p, |x| sext(x, $size).max(sext(v, $size)) as u32) }
            }
            #[unsafe(export_name = concat!("__sync_fetch_and_min_", $n))]
            pub unsafe extern "C" fn min(p: *mut u8, v: u32) -> u32 {
                unsafe { update(p, |x| sext(x, $size).min(sext(v, $size)) as u32) }
            }
            #[unsafe(export_name = concat!("__sync_fetch_and_umax_", $n))]
            pub unsafe extern "C" fn umax(p: *mut u8, v: u32) -> u32 {
                let m = if $size == 4 { u32::MAX } else { (1u32 << ($size * 8)) - 1 };
                unsafe { update(p, |x| x.max(v & m)) }
            }
            #[unsafe(export_name = concat!("__sync_fetch_and_umin_", $n))]
            pub unsafe extern "C" fn umin(p: *mut u8, v: u32) -> u32 {
                let m = if $size == 4 { u32::MAX } else { (1u32 << ($size * 8)) - 1 };
                unsafe { update(p, |x| x.min(v & m)) }
            }
        };
    };
}

sync_family!(1, "1");
sync_family!(2, "2");
sync_family!(4, "4");

// 64-bit: libatomic-style calls with a memory-order argument. One CPU, and
// every operation is fully ordered, so the order can be ignored.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __atomic_load_8(p: *const u64, _order: i32) -> u64 {
    unsafe { B::load64(p) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __atomic_store_8(p: *mut u64, v: u64, _order: i32) {
    unsafe { B::store64(p, v) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __atomic_exchange_8(p: *mut u64, v: u64, _order: i32) -> u64 {
    unsafe { rmw64::<B>(p, |_| v) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn __atomic_compare_exchange_8(
    p: *mut u64,
    expected: *mut u64,
    desired: u64,
    _success: i32,
    _failure: i32,
) -> bool {
    let exp = unsafe { *expected };
    let found = unsafe { B::cas64(p, exp, desired) };
    if found == exp {
        true
    } else {
        unsafe { *expected = found };
        false
    }
}

macro_rules! atomic8 {
    ($name:ident, $f:expr) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(p: *mut u64, v: u64, _order: i32) -> u64 {
            let f: fn(u64, u64) -> u64 = $f;
            unsafe { rmw64::<B>(p, |x| f(x, v)) }
        }
    };
}

atomic8!(__atomic_fetch_add_8, |x, v| x.wrapping_add(v));
atomic8!(__atomic_fetch_sub_8, |x, v| x.wrapping_sub(v));
atomic8!(__atomic_fetch_and_8, |x, v| x & v);
atomic8!(__atomic_fetch_or_8, |x, v| x | v);
atomic8!(__atomic_fetch_xor_8, |x, v| x ^ v);
atomic8!(__atomic_fetch_nand_8, |x, v| !(x & v));
