#![no_std]
#![no_main]

//! Interrupt demo: the BCM2835 legacy interrupt controller (0x3F00B200),
//! real 3-bank layout. The guest arms the system timer compare C1 (0x10 ->
//! CS bit 1 -> bank-1 bit 1, GPU IRQ 1) and the PL011 UART0 RX line (bank-2
//! bit 25 = IRQ 57 — RXINTR from the real register model), then spins with
//! interrupts unmasked; the host delivers the IRQ at a slice boundary
//! (ELR/SPSR saved, PC jumped to VBAR+0x280, DAIF.I set) and the handler
//! clears the source and re-arms.
//!
//! Vector table lives in the `.vectors` section at 0x100000 (linker.ld).

use pi_runtime::{putc, puts, putu};

const UART0: u32 = 0x3F20_1000;
const DR: u32 = UART0 + 0x00;
const FR: u32 = UART0 + 0x18;
const IMSC: u32 = UART0 + 0x38;
const ICR: u32 = UART0 + 0x44;

const FR_RXFE: u32 = 1 << 4;
const RXIM: u32 = 1 << 4;

const TMR_BASE: u32 = 0x3F00_3000;
const TMR_CS: u32 = TMR_BASE + 0x00;
const TMR_CLO: u32 = TMR_BASE + 0x04;
const TMR_C1: u32 = TMR_BASE + 0x10; // real C1: compare register 1

const IC_BASE: u32 = 0x3F00_B200;
const IC_PENDING1: u32 = IC_BASE + 0x04; // bank 1: timer C0-C3 bits 0-3
const IC_PENDING2: u32 = IC_BASE + 0x08; // bank 2: PL011 bit 25
const IC_ENABLE_IRQS1: u32 = IC_BASE + 0x10;
const IC_ENABLE_IRQS2: u32 = IC_BASE + 0x14;

const IRQ_TIMER1: u32 = 1 << 1; // system timer compare 1 (bank 1, bit 1)
const IRQ_UART: u32 = 1 << 25; // PL011 UART0 = IRQ 57 (bank 2, bit 25)

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

static mut IRQ_COUNT: u32 = 0;

// Vector table + entry glue. The table is in `.vectors` (0x100000,
// 128-byte aligned); the IRQ vector for "current EL, SPx" is VBAR+0x280.
// The host cannot redirect PC mid-flight in this emulator, so delivery is
// host-assisted: the host starts the next slice at the vector, and irq_glue
// writes the IRQ_RET magic after the handler so the host resumes the guest
// at the interrupted PC (a stand-in for the hardware's ELR/eret path).
core::arch::global_asm!(
    ".section .vectors,\"ax\",@progbits",
    "  b .", // 0x000 SP0 sync
    "  .org 0x080",
    "  b .", // 0x080 SP0 irq
    "  .org 0x100",
    "  b .", // 0x100 SP0 fiq
    "  .org 0x180",
    "  b .", // 0x180 SP0 err
    "  .org 0x200",
    "  b .", // 0x200 SPx sync
    "  .org 0x280",
    "  b irq_glue", // 0x280 SPx irq <- IRQ vector (EL1, SPx)
    "  .org 0x300",
    "  b .", // 0x300 SPx fiq
    "  .org 0x380",
    "  b .", // 0x380 SPx err
    "  .org 0x400",
    "  b .", // 0x400 aarch64 sync
    "  .org 0x480",
    "  b .", // 0x480 aarch64 irq
    "  .org 0x500",
    "  b .", // 0x500 aarch64 fiq
    "  .org 0x580",
    "  b .", // 0x580 aarch64 err
    "  .org 0x600",
    "  b .", // 0x600 aarch32 sync
    "  .org 0x680",
    "  b .", // 0x680 aarch32 irq
    "  .org 0x700",
    "  b .", // 0x700 aarch32 fiq
    "  .org 0x780",
    "  b .", // 0x780 aarch32 err
    "  .org 0x800",
    "irq_glue:",
    // The host cannot force a hardware context switch (ELR/SPSR are not
    // writable in this build), so the glue preserves the entire register
    // file: the handler's puts/putc clobber x0-x30, and the guest may be
    // interrupted in the middle of any puts loop (live string pointer).
    "  stp x0, x1, [sp, #-16]!",
    "  stp x2, x3, [sp, #-16]!",
    "  stp x4, x5, [sp, #-16]!",
    "  stp x6, x7, [sp, #-16]!",
    "  stp x8, x9, [sp, #-16]!",
    "  stp x10, x11, [sp, #-16]!",
    "  stp x12, x13, [sp, #-16]!",
    "  stp x14, x15, [sp, #-16]!",
    "  stp x16, x17, [sp, #-16]!",
    "  stp x18, x19, [sp, #-16]!",
    "  stp x20, x21, [sp, #-16]!",
    "  stp x22, x23, [sp, #-16]!",
    "  stp x24, x25, [sp, #-16]!",
    "  stp x26, x27, [sp, #-16]!",
    "  stp x28, x29, [sp, #-16]!",
    "  str x30, [sp, #-16]!",
    "  bl irq_handler_rust",
    // The IRQ_RET magic tells the host the handler is done; it clobbers
    // w0/w1, so write it before restoring the register file.
    "  movz w0, #1",
    "  movz w1, #0xb22c",
    "  movk w1, #0x3f00, lsl #16",
    "  str w0, [x1]",
    "  ldr x30, [sp], #16",
    "  ldp x28, x29, [sp], #16",
    "  ldp x26, x27, [sp], #16",
    "  ldp x24, x25, [sp], #16",
    "  ldp x22, x23, [sp], #16",
    "  ldp x20, x21, [sp], #16",
    "  ldp x18, x19, [sp], #16",
    "  ldp x16, x17, [sp], #16",
    "  ldp x14, x15, [sp], #16",
    "  ldp x12, x13, [sp], #16",
    "  ldp x10, x11, [sp], #16",
    "  ldp x8, x9, [sp], #16",
    "  ldp x6, x7, [sp], #16",
    "  ldp x4, x5, [sp], #16",
    "  ldp x2, x3, [sp], #16",
    "  ldp x0, x1, [sp], #16",
    "  b .",
);

#[no_mangle]
pub extern "C" fn irq_handler_rust() {
    let p1 = mmio_read(IC_PENDING1);
    let p2 = mmio_read(IC_PENDING2);
    if p1 & IRQ_TIMER1 != 0 {
        mmio_write(TMR_CS, 0); // clear compare-1 match (host sees the change)
        unsafe {
            IRQ_COUNT += 1;
            puts("[irq #");
            putu(IRQ_COUNT as u64);
            puts(" t+1s]\r\n");
        }
        mmio_write(TMR_C1, mmio_read(TMR_CLO).wrapping_add(1_000_000)); // re-arm
    }
    if p2 & IRQ_UART != 0 {
        // Read keys out of the PL011 RX FIFO until it empties (reading DR
        // pops it); the RXINTR line follows the FIFO, so draining it here
        // de-asserts the IRQ at the next slice boundary.
        while mmio_read(FR) & FR_RXFE == 0 {
            let ch = (mmio_read(DR) & 0xff) as u8;
            puts("[irq key: '");
            putc(ch);
            puts("']\r\n");
        }
        mmio_write(ICR, RXIM); // W1C: absorb (the line follows the FIFO)
    }
}

#[no_mangle]
#[unsafe(naked)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "movz w0, #0xfff0",
        "movk w0, #0x3f, lsl #16",
        "mov sp, x0",
        "b rust_main"
    )
}

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    puts("irq: BCM2835 interrupt controller @ 0x3F00B200\r\n");
    unsafe {
        core::arch::asm!(
            "mov x0, #0x100000",
            "msr vbar_el1, x0",
            "msr daifclr, #2",
            out("x0") _,
            options(nostack)
        );
        mmio_write(IMSC, RXIM); // arm the PL011 RXINTR
        mmio_write(IC_ENABLE_IRQS1, IRQ_TIMER1); // timer C1 -> bank-1 bit 1
        mmio_write(IC_ENABLE_IRQS2, IRQ_UART); // PL011 -> bank-2 bit 25
        mmio_write(TMR_C1, mmio_read(TMR_CLO).wrapping_add(1_000_000));
        puts("irq: timer C1 (IRQ 1) + UART RX (IRQ 57) armed\r\n");
        loop {
            core::arch::asm!("msr daifclr, #2", options(nostack));
            core::hint::spin_loop();
        }
    }
}