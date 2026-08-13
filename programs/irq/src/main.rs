#![no_std]
#![no_main]

//! Interrupt demo: the BCM2835 legacy interrupt controller (0x3F00B200).
//! The guest arms the system timer compare register C1 (IRQ 29) and the
//! PL011 UART RX line (IRQ 31), then spins with interrupts unmasked; the
//! host delivers the IRQ at a slice boundary (ELR/SPSR saved, PC jumped to
//! VBAR+0x280, DAIF.I set) and the handler clears the source and re-arms.
//!
//! Vector table lives in the `.vectors` section at 0x100000 (linker.ld).

use pi_runtime::{putc, puts, putu};

const UART: *mut u32 = 0x3F20_1000 as *mut u32;
const RX_SLOT: *mut u32 = (0x3F20_1000 + 0x80) as *mut u32;

const TMR_BASE: u32 = 0x3F00_3000;
const TMR_CS: u32 = TMR_BASE + 0x00;
const TMR_CLO: u32 = TMR_BASE + 0x04;
const TMR_C1: u32 = TMR_BASE + 0x14; // compare 1 (0x10 = compare 0!)

const IC_BASE: u32 = 0x3F00_B200;
const IC_PENDING1: u32 = IC_BASE + 0x04;
const IC_ENABLE_IRQS1: u32 = IC_BASE + 0x10;
const IC_IRQ_RET: u32 = IC_BASE + 0x2C; // host-assisted return (see below)

const IRQ_TIMER1: u32 = 1 << 29; // system timer compare 1
const IRQ_UART: u32 = 1 << 31; // PL011 UART

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
    "  bl irq_handler_rust",
    "  movz w0, #1",
    "  movz w1, #0xb22c",
    "  movk w1, #0x3f00, lsl #16",
    "  str w0, [x1]",
    "  b .",
);

#[no_mangle]
pub extern "C" fn irq_handler_rust() {
    let p = mmio_read(IC_PENDING1);
    if p & IRQ_TIMER1 != 0 {
        mmio_write(TMR_CS, 0); // clear compare-1 match (host sees the change)
        unsafe {
            IRQ_COUNT += 1;
            puts("[irq #");
            putu(IRQ_COUNT as u64);
            puts(" t+1s]\r\n");
        }
        mmio_write(TMR_C1, mmio_read(TMR_CLO).wrapping_add(1_000_000)); // re-arm
    }
    if p & IRQ_UART != 0 {
        let ch = unsafe { *RX_SLOT } as u8;
        unsafe { *RX_SLOT = 0 }; // consume the key
        puts("[irq key: '");
        putc(ch);
        puts("']\r\n");
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
            options(nostack)
        );
        mmio_write(IC_ENABLE_IRQS1, IRQ_TIMER1 | IRQ_UART);
        mmio_write(TMR_C1, mmio_read(TMR_CLO).wrapping_add(1_000_000));
        puts("irq: timer C1 + UART RX armed, IRQ enabled\r\n");
        loop {
            core::arch::asm!("msr daifclr, #2", options(nostack));
            core::hint::spin_loop();
        }
    }
}