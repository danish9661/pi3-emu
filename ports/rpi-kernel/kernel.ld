/* M52 kernel load address: the real Pi boot protocol puts the kernel
   Image at 0x80000 (text_offset), not the 0x100000 programs/ guests use.
   pi-cpu's load_elf honors e_entry, so _start lands at 0x80000 inside
   the 4 MB RAM. Stack at the top of RAM like the guests. */
ENTRY(_start)
SECTIONS {
  . = 0x80000;
  .vectors : ALIGN(2048) { KEEP(*(.vectors)) }
  .text : { KEEP(*(.text._start)) *(.text*) }
  .rodata : { *(.rodata*) *(.rodata.*) }
  . = ALIGN(0x10000);
  .data : { *(.data*) *(.got*) }
  .bss : { __bss_start = .; *(.bss*) *(COMMON) __bss_end = .; }
  /* M54: .data must stay within 4 MB RAM AND within ±4 GB of .text
     (adrp range). 0x90000 satisfies both and matches the tutorial's
     conventional placement; load_elf maps by p_vaddr so any RAM
     address works, but keep this one (statics live at 0x90020+). */
  . = 0x280000;
  .tables : { KEEP(*(.tables)) }
  /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}
