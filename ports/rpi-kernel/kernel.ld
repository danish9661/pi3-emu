/* M52 kernel load address: the real Pi boot protocol puts the kernel
   Image at 0x80000 (text_offset), not the 0x100000 programs/ guests use.
   pi-cpu's load_elf honors e_entry, so _start lands at 0x80000 inside
   the 4 MB RAM. Stack at the top of RAM like the guests. */
ENTRY(_start)
SECTIONS {
  . = 0x80000;
  .text : { KEEP(*(.text._start)) *(.text*) }
  .rodata : { *(.rodata*) *(.rodata.*) }
  .data : { *(.data*) *(.got*) }
  .bss : { __bss_start = .; *(.bss*) *(COMMON) __bss_end = .; }
  /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}
