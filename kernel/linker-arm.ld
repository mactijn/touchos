/* touchos kernel layout on 32-bit ARM (ARMv5, i.MX25/i.MX27).

   RAM (128 MiB at __ram_base) is mapped at __kernel_virt (0xc0000000):
   virtual = physical + __koff. The kernel is loaded at physical
   __ram_base + __kernel_offset (0x8000; by QEMU -kernel or OpenOCD
   load_image, both by load address) and entered at _start with the MMU
   off. So the .boot section (start-up code and the boot page table) is
   linked at its physical address; everything else at its virtual one.

   board.ld comes from build.rs, with the numbers of touchos_abi::imx_boot
   (which xtask loads by). */
INCLUDE board.ld
OUTPUT_FORMAT(elf32-littlearm)
ENTRY(_start)

__koff = __kernel_virt - __ram_base;

PHDRS
{
    boot   PT_LOAD FLAGS(7);
    text   PT_LOAD FLAGS(5);
    rodata PT_LOAD FLAGS(4);
    data   PT_LOAD FLAGS(6);
}

SECTIONS
{
    . = __ram_base + __kernel_offset;
    __kernel_phys_start = .;

    .boot : {
        KEEP(*(.text.boot))
        . = ALIGN(16384);
        __boot_l1 = .;          /* the first-level page table (16 KiB) */
        . += 16384;
    } :boot

    . = ALIGN(4096);
    . += __koff;

    .text : AT(ADDR(.text) - __koff) { *(.text .text.*) } :text

    . = ALIGN(4096);
    .rodata : AT(ADDR(.rodata) - __koff) { *(.rodata .rodata.*) } :rodata

    . = ALIGN(4096);
    .data : AT(ADDR(.data) - __koff) { *(.data .data.*) *(.got .got.*) } :data

    /* Not loaded by OpenOCD: the start-up code clears it. */
    .bss (NOLOAD) : AT(ADDR(.bss) - __koff) {
        . = ALIGN(8);
        __bss_start = .;
        *(.bss .bss.*) *(COMMON)
        . = ALIGN(8);
        __boot_stack_bottom = .;
        . += 0x8000;
        __boot_stack_top = .;
        __bss_end = .;
    } :data

    __kernel_end = .;

    /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) *(.eh_frame*) *(.note .note.*) *(.comment) }
}
