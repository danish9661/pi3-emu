ENTRY(_start)
SECTIONS {
  . = 0x100000;
  .vectors : ALIGN(128) { KEEP(*(.vectors)) }
  .text : { KEEP(*(.text._start)) *(.text*) }
  .rodata : { *(.rodata*) *(.rodata.*) }
  .data : { *(.data*) *(.got*) }
  .bss : { *(.bss*) *(COMMON) }
  . = 0x300000;
  .mbox : ALIGN(16) { KEEP(*(.mbox)) }
  /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}
