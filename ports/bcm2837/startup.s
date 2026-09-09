    .cpu cortex-a53
    .text
    .global _start
    .type _start, %function
// Guest entry: the host starts us at e_entry with SP unset, so set the
// stack top ourselves (RAM top, mirroring programs/runtime guests),
// zero .bss (no crt0: -nostartfiles), then enter C.
_start:
    movz x0, #0xfff0
    movk x0, #0x3f, lsl #16
    mov sp, x0
    adrp x0, __bss_start__
    add x0, x0, :lo12:__bss_start__
    adrp x1, __bss_end__
    add x1, x1, :lo12:__bss_end__
bss_loop:
    cmp x0, x1
    b.hs bss_done
    str xzr, [x0], #8
    b bss_loop
bss_done:
    bl main
hang:
    b hang
