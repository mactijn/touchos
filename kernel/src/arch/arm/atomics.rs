//! Restartable atomic helpers for user space (ARMv5 has no LDREX/STREX, and
//! user mode can't mask interrupts).
//!
//! Small compare-and-swap routines live in the vector page, at addresses
//! fixed by `touchos_atomics::helper`. Each one reads, compares, and then
//! commits with a single store. Only an interrupt can come between its
//! instructions (one CPU), and on every IRQ from user mode `rewind` moves a
//! PC that is past the start but not past the commit back to the start: the
//! routine runs again from scratch, so it behaves as one atomic step. It
//! uses no stack and only scratch registers, so starting over is harmless.

use super::syscall::SyscallFrame as TrapFrame;
use super::vectors::VECTORS;
use touchos_atomics::helper;

core::arch::global_asm!(
    r#"
    .section .text.kuser, "ax"
    .arm
    .global arm_kuser_cas32
    .global arm_kuser_cas32_commit
    .global arm_kuser_cas32_end
    .global arm_kuser_cas64
    .global arm_kuser_cas64_commit
    .global arm_kuser_cas64_end

    @ u32 cas32(u32 *p, u32 old, u32 new): stores new if *p == old; returns *p.
arm_kuser_cas32:
    ldr     r3, [r0]
    cmp     r3, r1
arm_kuser_cas32_commit:
    streq   r2, [r0]
    mov     r0, r3
    bx      lr
arm_kuser_cas32_end:

    @ u64 cas64(u64 *p, const u32 args[4]): args = old (lo, hi), new (lo, hi),
    @ 8-byte aligned. Stores new if *p == old; returns what *p was.
arm_kuser_cas64:
    ldrd    r2, r3, [r0]
    ldr     r12, [r1]
    cmp     r2, r12
    ldreq   r12, [r1, #4]
    cmpeq   r3, r12
    bne     1f
    ldrd    r2, r3, [r1, #8]
arm_kuser_cas64_commit:
    strd    r2, r3, [r0]
    ldr     r0, [r1]                @ it was old
    ldr     r1, [r1, #4]
    bx      lr
1:  mov     r0, r2
    mov     r1, r3
    bx      lr
arm_kuser_cas64_end:
"#
);

unsafe extern "C" {
    static arm_kuser_cas32: u8;
    static arm_kuser_cas32_commit: u8;
    static arm_kuser_cas32_end: u8;
    static arm_kuser_cas64: u8;
    static arm_kuser_cas64_commit: u8;
    static arm_kuser_cas64_end: u8;
}

/// (start in the kernel image, commit, end).
fn helpers() -> [(usize, usize, usize, usize); 2] {
    [
        (
            helper::CAS32,
            &raw const arm_kuser_cas32 as usize,
            &raw const arm_kuser_cas32_commit as usize,
            &raw const arm_kuser_cas32_end as usize,
        ),
        (
            helper::CAS64,
            &raw const arm_kuser_cas64 as usize,
            &raw const arm_kuser_cas64_commit as usize,
            &raw const arm_kuser_cas64_end as usize,
        ),
    ]
}

/// Copy the helpers into the vector page (`page` = its kernel address).
pub fn install(page: *mut u8) {
    for (addr, start, _, end) in helpers() {
        let off = addr - VECTORS as usize;
        assert!(off + (end - start) <= 0x1000);
        unsafe { core::ptr::copy_nonoverlapping(start as *const u8, page.add(off), end - start) };
    }
}

/// On an IRQ from user mode: restart a helper that hasn't committed.
pub fn rewind(f: &mut TrapFrame) {
    if f.spsr & 0x1f != 0x10 {
        return;
    }
    let pc = f.pc as usize;
    for (addr, start, commit, _) in helpers() {
        if pc > addr && pc <= addr + (commit - start) {
            f.pc = addr as u32;
        }
    }
}
