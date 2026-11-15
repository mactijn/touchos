//! Exceptions: the vector page at 0xffff0000 (high vectors) and the entry
//! and exit code.
//!
//! Every exception ends up in SVC mode with a `TrapFrame` on the current
//! thread's kernel (SVC) stack: r0-r12, the interrupted mode's sp and lr,
//! the return address and the saved CPSR, plus a padding word (72 bytes,
//! so the stack stays 8-byte aligned for LDRD/STRD). IRQ, abort and undefined-
//! instruction exceptions arrive in their own CPU modes; those only park
//! r0, the return address and the SPSR in a 3-word scratch area (their
//! banked sp) and switch to SVC mode right away. `trap_return` goes back to
//! user or kernel mode from the frame.
//!
//! The vector page is readable (and so executable) from user mode: the
//! restartable atomic helpers live in it too (see `touchos-atomics`).

use super::cache;
use super::syscall::SyscallFrame as TrapFrame;
use crate::mem::{self, PAGE};

pub const VECTORS: u64 = 0xffff_0000;

core::arch::global_asm!(
    r#"
    .section .text.vectors, "ax"
    .arm
    .global arm_vector_code
    .global arm_vector_code_end
    @ Copied to 0xffff0000: each entry jumps through the address table that
    @ follows (pc reads 8 ahead: entry + 8 + 0x18 = entry + 0x20).
arm_vector_code:
    ldr pc, [pc, #0x18]     @ reset
    ldr pc, [pc, #0x18]     @ undefined instruction
    ldr pc, [pc, #0x18]     @ svc
    ldr pc, [pc, #0x18]     @ prefetch abort
    ldr pc, [pc, #0x18]     @ data abort
    ldr pc, [pc, #0x18]     @ (unused)
    ldr pc, [pc, #0x18]     @ irq
    ldr pc, [pc, #0x18]     @ fiq
    .word arm_vec_reset
    .word arm_vec_undef
    .word arm_vec_svc
    .word arm_vec_pabort
    .word arm_vec_dabort
    .word arm_vec_reset
    .word arm_vec_irq
    .word arm_vec_fiq
arm_vector_code_end:

    .section .text.traps, "ax"
    .arm

    @ In an exception mode with lr = return address: park r0, lr and spsr in
    @ the mode's scratch area, switch to SVC mode, build the frame, call
    @ \handler(frame, \kind).
    .macro from_mode handler, kind
    stmia   sp, {{r0, lr}}
    mrs     r0, spsr
    str     r0, [sp, #8]
    mov     r0, sp
    msr     cpsr_c, #0xd3           @ SVC mode, IRQ and FIQ masked
    sub     sp, sp, #72
    stmib   sp, {{r1-r12}}
    ldmia   r0, {{r1, r2, r3}}      @ r0, return pc, spsr of the interrupted code
    str     r1, [sp]
    add     r4, sp, #60
    stmia   r4, {{r2, r3}}
    and     r1, r3, #0x1f
    cmp     r1, #0x10               @ from user mode?
    addeq   r4, sp, #52
    stmiaeq r4, {{sp, lr}}^         @ the user's sp and lr
    addne   r1, sp, #72             @ else the kernel's: sp before the frame
    strne   r1, [sp, #52]
    strne   lr, [sp, #56]
    mov     r0, sp
    mov     r1, #\kind
    bl      \handler
    b       arm_trap_return
    .endm

    @ Everything from here to arm_entry_end builds trap frames on the SVC
    @ stack. A data abort in there means that stack is gone: a kernel stack
    @ overflow into its guard page (the first abort's frame doesn't fit
    @ either). Building another frame would just fault again, in a loop.
    .global arm_vec_irq
    .global arm_vec_dabort
    .global arm_vec_undef
    .global arm_vec_reset
arm_entry_start:
arm_vec_irq:
    sub     lr, lr, #4
    from_mode arm_irq, 0
arm_vec_dabort:
    sub     lr, lr, #8
    ldr     sp, =arm_entry_start    @ sp is free here: the scratch area's
    cmp     lr, sp                  @ address is reloaded below
    ldr     sp, =arm_entry_end
    cmphs   sp, lr                  @ HI: start <= lr < end
    bhi     arm_nested_abort
    ldr     sp, =ARM_EXC_SCRATCH + 16
    from_mode arm_fault, 1
arm_vec_pabort:
    sub     lr, lr, #4
    from_mode arm_fault, 2
arm_vec_undef:
    sub     lr, lr, #4
    from_mode arm_fault, 3
arm_vec_reset:
    sub     lr, lr, #4
    from_mode arm_fault, 4
arm_entry_end:

    @ Still in abort mode, with the first exception's r0, pc and spsr
    @ untouched in its mode's scratch area: report it from a stack of its
    @ own. r0 = where this abort hit (which entry, so which scratch area).
arm_nested_abort:
    ldr     sp, =ARM_EMERGENCY_STACK + 8192
    mov     r0, lr
    bl      arm_kstack_overflow
    .ltorg
arm_vec_fiq:
    subs    pc, lr, #4              @ FIQs are never enabled

    @ svc #0: already in SVC mode, lr = the next instruction.
arm_vec_svc:
    sub     sp, sp, #72
    stmia   sp, {{r0-r12}}
    add     r8, sp, #52
    stmia   r8, {{sp, lr}}^         @ the user's sp and lr
    nop
    str     lr, [sp, #60]
    mrs     r8, spsr
    str     r8, [sp, #64]
    mov     r0, sp
    bl      arm_syscall
    @ fall through

    .global arm_trap_return
arm_trap_return:                    @ sp = the frame
    ldr     r0, [sp, #64]
    msr     spsr_cxsf, r0
    and     r0, r0, #0x1f
    cmp     r0, #0x10
    beq     1f
    ldmia   sp, {{r0-pc}}^          @ to the kernel: sp, lr, pc and CPSR
1:  add     r0, sp, #52
    ldmia   r0, {{sp, lr}}^         @ the user's sp and lr
    nop
    ldr     lr, [sp, #60]
    ldmia   sp, {{r0-r12}}
    add     sp, sp, #72
    movs    pc, lr                  @ to user mode
"#
);

unsafe extern "C" {
    static arm_vector_code: u8;
    static arm_vector_code_end: u8;
}

/// 3-word scratch areas: the banked sp of the IRQ, abort and undefined
/// modes (the data-abort entry reloads the abort mode's by name).
#[unsafe(no_mangle)]
static mut ARM_EXC_SCRATCH: [[u32; 4]; 3] = [[0; 4]; 3];

/// The stack `arm_kstack_overflow` reports from.
#[unsafe(no_mangle)]
static mut ARM_EMERGENCY_STACK: [u64; 1024] = [0; 1024];

/// Give the exception modes their scratch areas, put the vector page at
/// 0xffff0000 (user read-only) and switch to high vectors.
pub fn init() {
    let s = &raw mut ARM_EXC_SCRATCH as usize as u32;
    unsafe {
        core::arch::asm!(
            "mrs {old}, cpsr",
            "msr cpsr_c, #0xd2", "mov sp, {irq}",   // IRQ mode
            "msr cpsr_c, #0xd7", "mov sp, {abt}",   // abort mode
            "msr cpsr_c, #0xdb", "mov sp, {und}",   // undefined mode
            "msr cpsr_c, {old}",
            old = out(reg) _,
            irq = in(reg) s,
            abt = in(reg) s + 16,
            und = in(reg) s + 32,
            options(nostack),
        )
    };

    // The page itself: the code above, then (later) the atomic helpers.
    let frame = mem::alloc_frame();
    let dst = mem::phys_to_virt(frame);
    let (start, end) = (&raw const arm_vector_code, &raw const arm_vector_code_end);
    let len = end as usize - start as usize;
    unsafe { core::ptr::copy_nonoverlapping(start, dst, len) };
    super::atomics::install(dst);
    cache::clean_invalidate_dcache(dst as usize, PAGE as usize);
    super::paging::map_vectors(frame);
    cache::invalidate_icache();
    unsafe {
        core::arch::asm!(
            "mrc p15, 0, {t}, c1, c0, 0",
            "orr {t}, {t}, #0x2000",               // V: vectors at 0xffff0000
            "mcr p15, 0, {t}, c1, c0, 0",
            t = out(reg) _,
            options(nostack),
        )
    };
}

fn from_user(f: &TrapFrame) -> bool {
    f.spsr & 0x1f == 0x10
}

#[unsafe(no_mangle)]
extern "C" fn arm_irq(f: &mut TrapFrame, _kind: u32) {
    super::atomics::rewind(f);
    crate::board::irq::dispatch(from_user(f));
}

#[unsafe(no_mangle)]
extern "C" fn arm_syscall(f: &mut TrapFrame) {
    crate::sys::dispatch(f);
}

/// Fault status and address registers (CP15 c5, c6).
fn fsr_far() -> (u32, u32) {
    let (fsr, far): (u32, u32);
    unsafe {
        core::arch::asm!(
            "mrc p15, 0, {s}, c5, c0, 0",
            "mrc p15, 0, {a}, c6, c0, 0",
            s = out(reg) fsr,
            a = out(reg) far,
            options(nomem, nostack, preserves_flags),
        )
    };
    (fsr, far)
}

unsafe extern "C" {
    static arm_vec_irq: u8;
    static arm_vec_dabort: u8;
    static arm_vec_undef: u8;
    static arm_vec_reset: u8;
}

/// A data abort while building a trap frame (at `at`, inside one of the
/// entries): the SVC stack overflowed. The first exception is the one whose
/// entry that was; its pc is in its mode's scratch area.
#[unsafe(no_mangle)]
extern "C" fn arm_kstack_overflow(at: u32) -> ! {
    let a = |s: &u8| s as *const u8 as u32;
    let (what, area) = unsafe {
        if at < a(&arm_vec_dabort) && at >= a(&arm_vec_irq) {
            ("an interrupt", 0)
        } else if at >= a(&arm_vec_undef) && at < a(&arm_vec_reset) {
            ("an undefined instruction", 2)
        } else {
            ("an abort", 1)
        }
    };
    let first = unsafe { (*(&raw const ARM_EXC_SCRATCH))[area] };
    let (_, far) = fsr_far();
    panic!(
        "kernel stack overflow: {what} at pc {:#010x}, then a data abort at {far:#010x} while saving it",
        first[1]
    );
}

/// Test (`test_fail=kstack`): run into the guard page below `stack_base`.
pub fn overflow_into_guard(stack_base: usize) -> ! {
    unsafe {
        core::arch::asm!(
            "mov sp, {b}",
            "1: push {{r0-r3}}",
            "b 1b",
            b = in(reg) stack_base + 64,
            options(noreturn),
        )
    }
}

#[unsafe(no_mangle)]
extern "C" fn arm_fault(f: &mut TrapFrame, kind: u32) {
    let (what, vector) = match kind {
        1 => ("data abort", 4),
        2 => ("prefetch abort", 3),
        3 => ("undefined instruction", 1),
        _ => ("jump to 0", 0),
    };
    let (fsr, far) = fsr_far();
    let addr = matches!(kind, 1 | 2).then_some(if kind == 1 { far as u64 } else { f.pc as u64 });
    if from_user(f) {
        crate::task::user_fault(
            what,
            vector,
            (f.pc as u64, f.sp_usr as u64),
            (kind == 1).then_some(fsr as u64),
            addr,
        );
    }
    panic!(
        "{what} in the kernel at pc {:#010x}, lr {:#010x}, sp {:#010x}, address {:#010x}, status {fsr:#x}",
        f.pc, f.lr_usr, f.sp_usr, far
    );
}
