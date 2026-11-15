/* The boot stage runs at 0xa7000000, out of the way of everything it
   loads (kernel 0xa0008000, boot info and config 0xa1000000, the
   framebuffer 0xa1400000, the initrd 0xa2000000) and of U-Boot (0xa1f00000).
   Entry points first: +0 boot, +4 flash, +8 probe. */
ENTRY(_start)
SECTIONS
{
  . = 0xa7000000;
  .text   : { KEEP(*(.text.start)) *(.text .text.*) }
  .rodata : { *(.rodata .rodata.*) }
  .data   : { *(.data .data.*) }
  .bss (NOLOAD) : { __bss_start = .; *(.bss .bss.*) *(COMMON) __bss_end = .; }
  /DISCARD/ : { *(.ARM.exidx*) *(.ARM.extab*) }
}
