#![no_std]
#![no_main]

//! irqcore: proves the patched core's real exception path.
//!
//! The guest installs a vector table, clears DAIF.I and spins with the
//! physical arch timer armed (TVAL=0x1000, CTL.ENABLE set, IMASK clear).
//! The host raises the IRQ line (uc_arm64_set_irq) or advances the timer
//! counter (uc_arm64_timer_tick); the CPU then takes a REAL exception:
//! ELR_EL1/SPSR_EL1 saved, PC -> VBAR+0x280, DAIF.I set. The handler
//! records ELR, SPSR, CNTP_CTL (ISTATUS), CNTPCT into SCRATCH, disables
//! the timer (level de-assert), and eret's back to the interrupted PC.

use core::panic::PanicInfo;

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {}
}

// [0]=ELR_EL1 [1]=SPSR_EL1 [2]=CNTP_CTL_EL0 [3]=CNTPCT_EL0 [4]=seen flag
#[no_mangle]
pub static mut SCRATCH: [u64; 5] = [0; 5];

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
    "  mrs x1, elr_el1",
    "  adrp x0, SCRATCH",
    "  add x0, x0, :lo12:SCRATCH",
    "  str x1, [x0]", // SCRATCH[0] = ELR_EL1
    "  mrs x1, spsr_el1",
    "  str x1, [x0, #8]", // SCRATCH[1] = SPSR_EL1
    "  mrs x1, cntp_ctl_el0",
    "  str x1, [x0, #16]", // SCRATCH[2] = CNTP_CTL_EL0 (ISTATUS bit 2)
    "  mrs x1, cntpct_el0",
    "  str x1, [x0, #24]", // SCRATCH[3] = CNTPCT_EL0
    "  mov x1, #1",
    "  str x1, [x0, #32]", // SCRATCH[4] = seen flag
    "  msr cntp_ctl_el0, xzr", // disable timer: level de-asserts
    "  msr daifset, #2", // belt-and-braces (hw already masked)
    "  eret",
);

#[no_mangle]
#[unsafe(naked)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "movz x0, #0xf000",
        "movk x0, #0x3, lsl #16",
        "mov sp, x0",
        "b rust_main"
    )
}

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    unsafe {
        core::arch::asm!(
            "mov x0, #0x100000",
            "msr vbar_el1, x0",
            "msr daifclr, #2",
            out("x0") _,
            options(nostack)
        );
        // arm the physical timer: cval = cntpct + 0x1000, CTL = ENABLE
        core::arch::asm!(
            "mov x0, #0x1000",
            "msr cntp_tval_el0, x0",
            "mov x0, #1",
            "msr cntp_ctl_el0, x0",
            options(nostack)
        );
        loop {
            core::arch::asm!("msr daifclr, #2", options(nostack));
            core::hint::spin_loop();
        }
    }
}
