//! Start-up: from wherever the loader left the CPU to Rust at 0xc0000000.
//!
//! QEMU (`-kernel`) and OpenOCD (`load_image`, after halting U-Boot) both
//! load the kernel at its physical load addresses and jump to `_start`. Its
//! section is linked at its physical address (see linker-arm.ld). It:
//! 1. masks interrupts, writes back and turns off whatever caches and MMU
//!    U-Boot had on;
//! 2. builds the boot page table from 1 MiB sections: RAM at its physical
//!    address (for the next few instructions) and at 0xc0000000
//!    (`imx_boot::KERNEL_VIRT`: `__kernel_virt` in board.ld), and the
//!    UART's MiB uncached at 0xfe000000 (the early console);
//! 3. turns on the MMU, caches and alignment checking;
//! 4. clears .bss (OpenOCD loads only file contents) and calls
//!    `arm_kmain(boot_l1)` on the boot stack.

core::arch::global_asm!(
    r#"
    .section .text.boot, "ax"
    .global _start
    .arm
_start:
    msr     cpsr_c, #0xd3              @ SVC mode, IRQ and FIQ masked
1:  mrc     p15, 0, APSR_nzcv, c7, c14, 3 @ ARM926: test, clean and invalidate D-cache
    bne     1b
    mov     r0, #0
    mcr     p15, 0, r0, c7, c10, 4     @ drain the write buffer
    mrc     p15, 0, r0, c1, c0, 0
    bic     r0, r0, #0x0005            @ M (MMU), C (D-cache)
    bic     r0, r0, #0x1000            @ I (I-cache)
    bic     r0, r0, #0x2000            @ V (vectors at 0 for now)
    mcr     p15, 0, r0, c1, c0, 0
    mov     r0, #0
    mcr     p15, 0, r0, c7, c7, 0      @ invalidate I and D caches
    mcr     p15, 0, r0, c8, c7, 0      @ invalidate the TLBs

    @ Clear the boot page table (16 KiB).
    ldr     r4, =__boot_l1
    mov     r0, #0
    mov     r1, r4
    add     r2, r4, #0x4000
2:  str     r0, [r1], #4
    cmp     r1, r2
    blo     2b

    @ RAM (RAM_SIZE: 128 MiB) at its physical address and at 0xc0000000.
    ldr     r0, =__ram_base
    ldr     r5, =__koff
    ldr     r1, =0x41e                 @ section, AP=01 (kernel RW), cached, buffered
    mov     r2, #{ram_mib}             @ that many 1 MiB sections
3:  orr     r3, r0, r1
    str     r3, [r4, r0, lsr #18]
    add     r6, r0, r5
    str     r3, [r4, r6, lsr #18]
    add     r0, r0, #0x100000
    subs    r2, r2, #1
    bne     3b

    @ The UART's MiB, uncached and unbuffered, at EARLY_UART_VIRT.
    ldr     r0, =__uart_phys
    mov     r0, r0, lsr #20
    mov     r0, r0, lsl #20
    ldr     r1, =0x412                 @ section, AP=01
    orr     r0, r0, r1
    ldr     r1, ={uart_l1}             @ its first-level entry
    str     r0, [r4, r1]

    mov     r0, #1                     @ domain 0: client (the AP bits apply)
    mcr     p15, 0, r0, c3, c0, 0
    mcr     p15, 0, r4, c2, c0, 0      @ translation table base
    mrc     p15, 0, r0, c1, c0, 0
    ldr     r1, =0x1007                @ M, A (alignment faults), C, I
    orr     r0, r0, r1
    mcr     p15, 0, r0, c1, c0, 0
    nop
    nop

    @ From here on the high addresses work.
    ldr     sp, =__boot_stack_top
    ldr     r0, =__bss_start
    ldr     r1, =__bss_end
    mov     r2, #0
4:  cmp     r0, r1
    strlo   r2, [r0], #4
    blo     4b
    mov     r0, r4                     @ arm_kmain(boot_l1 physical address)
    ldr     pc, =arm_kmain
    .ltorg
"#,
    ram_mib = const touchos_abi::imx_boot::RAM_SIZE >> 20,
    uart_l1 = const (EARLY_UART_VIRT >> 20) * 4,
);

/// Where the boot page table maps the UART's MiB (board/imx/serial.rs
/// writes the console through it until paging is set up for good).
pub const EARLY_UART_VIRT: u64 = 0xfe00_0000;
