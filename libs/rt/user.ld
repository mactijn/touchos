/* Linker script for touchos user programs (32-bit ARM): static, non-PIE,
   at IMAGE_BASE (touchos-abi layout::IMAGE_BASE). One PT_LOAD per
   permission. (ARMv5 has no execute-never bit, so W^X only holds for
   writes.) */
OUTPUT_FORMAT(elf32-littlearm)
ENTRY(_start)

PHDRS
{
    text   PT_LOAD FLAGS(5);  /* R X */
    rodata PT_LOAD FLAGS(4);  /* R   */
    data   PT_LOAD FLAGS(6);  /* R W */
}

SECTIONS
{
    . = 0x400000;

    .text : {
        KEEP(*(.text._start))
        *(.text .text.*)
    } :text

    . = ALIGN(4096);
    .rodata : { *(.rodata .rodata.*) } :rodata

    . = ALIGN(4096);
    .data : { *(.data .data.*) } :data
    .got  : { *(.got .got.*) } :data
    .bss  : { *(.bss .bss.*) *(COMMON) } :data

    /DISCARD/ : { *(.eh_frame*) *(.ARM.exidx*) *(.ARM.extab*) *(.ARM.attributes)
                  *(.note .note.*) *(.comment) *(.interp) *(.dynamic) }
}
