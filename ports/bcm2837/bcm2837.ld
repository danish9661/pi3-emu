ENTRY(_start)
SECTIONS {
  . = 0x100000;
  .vectors : ALIGN(128) { KEEP(*(.vectors)) }
  .text : { KEEP(*(.text._start)) *(.text*) }
  .rodata : { *(.rodata*) *(.rodata.*) }
  .data : { *(.data*) *(.got*) }
  .bss : { *(.bss*) *(COMMON) }
  __bss_start__ = ADDR(.bss);
  __bss_end__ = ADDR(.bss) + SIZEOF(.bss);
  /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}
