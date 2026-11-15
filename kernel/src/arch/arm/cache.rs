//! ARM926EJ-S cache and TLB maintenance (CP15 c7 and c8). The D-cache is
//! write-back and virtually indexed and tagged, with 32-byte lines; the
//! write buffer must be drained before memory is seen by the MMU or DMA.

use core::arch::asm;

const LINE: usize = 32;

fn lines(va: usize, len: usize) -> impl Iterator<Item = usize> {
    let start = va & !(LINE - 1);
    (start..va + len).step_by(LINE)
}

/// Write dirty lines in `[va, va+len)` back to memory (they stay cached).
pub fn clean_dcache(va: usize, len: usize) {
    for l in lines(va, len) {
        unsafe { asm!("mcr p15, 0, {l}, c7, c10, 1", l = in(reg) l, options(nostack)) };
    }
    drain_write_buffer();
}

/// Write back and drop the lines in `[va, va+len)`.
pub fn clean_invalidate_dcache(va: usize, len: usize) {
    for l in lines(va, len) {
        unsafe { asm!("mcr p15, 0, {l}, c7, c14, 1", l = in(reg) l, options(nostack)) };
    }
    drain_write_buffer();
}

/// Write back and drop the whole D-cache, drop the I-cache.
pub fn flush_all() {
    unsafe {
        asm!(
            "1: mrc p15, 0, APSR_nzcv, c7, c14, 3", // test, clean and invalidate
            "bne 1b",
            "mcr p15, 0, {z}, c7, c5, 0",           // invalidate the I-cache
            "mcr p15, 0, {z}, c7, c10, 4",          // drain the write buffer
            z = in(reg) 0u32,
            options(nostack),
        )
    };
}

/// The I-cache after writing code (the vectors page).
pub fn invalidate_icache() {
    unsafe { asm!("mcr p15, 0, {z}, c7, c5, 0", z = in(reg) 0u32, options(nostack)) };
}

pub fn drain_write_buffer() {
    unsafe { asm!("mcr p15, 0, {z}, c7, c10, 4", z = in(reg) 0u32, options(nostack)) };
}

pub fn invalidate_tlb() {
    unsafe { asm!("mcr p15, 0, {z}, c8, c7, 0", z = in(reg) 0u32, options(nostack)) };
}

pub fn invalidate_tlb_entry(va: usize) {
    unsafe { asm!("mcr p15, 0, {v}, c8, c7, 1", v = in(reg) va, options(nostack)) };
}
