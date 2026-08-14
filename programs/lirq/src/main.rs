#![no_std]
#![no_main]

//! lirq: Phase 2 — real MMIO IRQ semantics through the BCM2836 local
//! interrupt block (0x40000000).
//!
//! The guest installs a vector table, clears DAIF.I and spins. Unlike
//! irqcore (where the host magic is invisible to the guest), the delivery
//! source is visible in the local block's per-core IRQ source register
//! (0x40000060, core 0): bit 1 = CNTPNSIRQ (arch timer) and bit 8 = GPU
//! (the legacy IC's pending-and-enabled lines, routed to core 0 by
//! GPU_ROUTING).
//!
//! Phase A — CNTPNS: arm the physical timer; the host advances the counter;
//! the handler reads the source register (must show bit 1), records
//! ELR/SPSR/CNTPCT, disables the timer (level de-assert) and eret's.
//!
//! Phase B — GPU: enable system-timer C1 in the legacy IC (basic bank bit
//! 29), program CMP1 = CLO + 1s; the C1 match drives the IC line into the
//! local block's GPU bit; the handler reads the source register (must show
//! bit 8), acks by clearing TMR_CS, and eret's. No re-entry either phase.

use pi_runtime::puts;

// [0]=ELR(A) [1]=SPSR(A) [2]=source(A) [3]=CNTPCT(A) [4]=flag(A:1)
// [5]=ELR(B) [6]=SPSR(B) [7]=source(B) [8]=TMR_CS(B) [9]=flag(B:2)
#[no_mangle]
pub static mut SCRATCH: [u64; 10] = [0; 10];

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
    "  mrs x2, spsr_el1",
    "  adrp x0, SCRATCH",
    "  add x0, x0, :lo12:SCRATCH",
    "  ldr x3, [x0, #32]", // flag(A)
    "  cbnz x3, phase_b", // A done already -> this must be the GPU IRQ
    // Phase A: CNTPNSIRQ
    "  str x1, [x0]",
    "  str x2, [x0, #8]",
    "  movz x3, #0x0060",
    "  movk x3, #0x4000, lsl #16",
    "  ldr w4, [x3]", // 0x40000060: local IRQ source, core 0 (32-bit cell)
    "  str x4, [x0, #16]",
    "  mrs x4, cntpct_el0",
    "  str x4, [x0, #24]",
    "  mov x3, #1",
    "  str x3, [x0, #32]", // flag(A) = 1
    "  msr cntp_ctl_el0, xzr", // ack: disable the timer, level de-asserts
    "  b irq_done",
    "phase_b:",
    "  str x1, [x0, #40]",
    "  str x2, [x0, #48]",
    "  movz x3, #0x0060",
    "  movk x3, #0x4000, lsl #16",
    "  ldr w4, [x3]",
    "  str x4, [x0, #56]", // source(B)
    "  movz x3, #0x3000",
    "  movk x3, #0x3f00, lsl #16",
    "  ldr w4, [x3]", // TMR_CS read-back
    "  str x4, [x0, #64]",
    "  str wzr, [x3]", // ack B: clear the match bits, GPU line drops
    "  mov x3, #2",
    "  str x3, [x0, #72]", // flag(B) = 2
    "irq_done:",
    "  msr daifset, #2",
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

const IC_BASE: u32 = 0x3F00_B200;
const IC_ENABLE_BASIC: u32 = IC_BASE + 0x18;
const IRQ_TIMER1: u32 = 1 << 29; // system timer compare 1
const TMR_BASE: u32 = 0x3F00_3000;
const TMR_CLO: u32 = TMR_BASE + 0x04;
// The host timer model maps compare register i -> CS bit i (the irq guest's
// C1 convention): the compare at +0x14 (index 2) drives CS bit 2, which is
// the line main.js maps to IRQ 29 in the legacy IC.
const TMR_C1: u32 = TMR_BASE + 0x14;

fn mmio_read(addr: u32) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}

fn mmio_write(addr: u32, v: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, v) }
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
        // Phase A: arm the physical timer (cval = cntpct + 0x1000)
        core::arch::asm!(
            "mov x0, #0x1000",
            "msr cntp_tval_el0, x0",
            "mov x0, #1",
            "msr cntp_ctl_el0, x0",
            options(nostack)
        );
        while SCRATCH[4] != 1 {
            core::arch::asm!("msr daifclr, #2", options(nostack));
            core::hint::spin_loop();
        }
        // Phase B: enable the timer line in the legacy IC, program the
        // compare = CLO + 1s
        mmio_write(IC_ENABLE_BASIC, IRQ_TIMER1);
        let clo = mmio_read(TMR_CLO);
        mmio_write(TMR_C1, clo + 0xF4240);
        while SCRATCH[9] != 2 {
            core::arch::asm!("msr daifclr, #2", options(nostack));
            core::hint::spin_loop();
        }
        puts("lirq: A and B delivered\n");
        loop {
            core::arch::asm!("msr daifclr, #2", options(nostack));
            core::hint::spin_loop();
        }
    }
}
