#![no_std]
#![no_main]

//! DMA demo: the BCM2835 DMA controller (0x3F007000).
//!
//! The guest programs a 3-control-block chain in RAM, enables DMA channel 0
//! (ENABLE register, like the real chip), arms IRQ 16 (DMA0) in the legacy
//! interrupt controller, then starts the chain with CS.ACTIVE. The host
//! performs the transfers between slices (src/dma.js), latches CS.END and
//! raises CS.INT (the final CB has the INTEN bit), which drives the IC's
//! DMA0 line — the M11 delivery path vectors the guest and the handler
//! prints, then the guest verifies all three destinations.
//!
//!   CB0 0x284000: copy 64 bytes 0x285000 -> 0x286000 (pattern)
//!   CB1 0x284020: copy 32 bytes 0x286000 -> 0x287000 (relay)
//!   CB2 0x284040: fill 16 bytes at 0x288000 with the byte at 0x284080
//!                (SRC_IGNORE, last CB sets INTEN -> completion IRQ)

use pi_runtime::puts;

const IC_BASE: u32 = 0x3F00_B200;
const IC_PENDING1: u32 = IC_BASE + 0x04;
const IC_ENABLE_IRQS1: u32 = IC_BASE + 0x10;

const DMA_BASE: u32 = 0x3F00_7000;
const DMA_CS: u32 = DMA_BASE + 0x00;
const DMA_CONBLK: u32 = DMA_BASE + 0x04;
const DMA_ENABLE: u32 = 0x3F00_E050;
const DMA_DONE: u32 = 0x3F00_E054; // host extension, like TMR_DONE/MMU_DONE

const IRQ_DMA0: u32 = 1 << 16;

const SRC: u32 = 0x285000; // 64-byte pattern
const DST: u32 = 0x286000; // full copy
const DST2: u32 = 0x287000; // relay copy (32 bytes)
const DST3: u32 = 0x288000; // SRC_IGNORE fill (16 bytes)
const FILL_BYTE: u32 = 0x284080;

const CB0: u32 = 0x284000;
const CB1: u32 = 0x284020;
const CB2: u32 = 0x284040;

// TI bits (real: SRC_INC 0, DEST_INC 1, SRC_IGNORE 6; INTEN 31 is a host
// extension documented in src/dma.js).
const TI_COPY: u32 = 0b11;
const TI_FILL: u32 = (1 << 6) | 0b10 | (1 << 31); // SRC_IGNORE | DEST_INC | INTEN

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

// Vector table + entry glue, same pattern as the irq program (M11).
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
    "  stp x29, x30, [sp, #-16]!", // preserve the guest's frame pointer/LR
    "  bl irq_handler_rust",
    "  ldp x29, x30, [sp], #16", // the bl above clobbers x30; the guest's
    "  movz w0, #1", //             resumed code must not `ret` into the glue
    "  movz w1, #0xb22c",
    "  movk w1, #0x3f00, lsl #16",
    "  str w0, [x1]",
    "  b .",
);

#[no_mangle]
pub extern "C" fn irq_handler_rust() {
    let p = mmio_read(IC_PENDING1);
    if p & IRQ_DMA0 != 0 {
        puts("[dma irq] completed\r\n");
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

// Poll CS until END (bit 1) is set, with a bounded spin. The loop state
// lives in callee-saved registers (x19/x20) so the DMA completion IRQ can
// fire mid-poll without corrupting it; the handler clobbers x0-x18 freely.
#[no_mangle]
#[unsafe(naked)]
pub extern "C" fn poll_end(_cs: u32) -> u32 {
    core::arch::naked_asm!(
        "stp x19, x20, [sp, #-16]!", // x19/x20 are callee-saved: preserve them
        "mov x19, x0",
        "movz x20, #0xd40",
        "movk x20, #0x3, lsl #16",
        "1:",
        "ldr w2, [x19]",
        "tbz w2, #1, 2f",
        "mov w0, #1",
        "ldp x19, x20, [sp], #16",
        "ret",
        "2:",
        "subs x20, x20, #1",
        "bne 1b",
        "mov w0, #0",
        "ldp x19, x20, [sp], #16",
        "ret"
    )
}

// Bounded spin whose only live state is a counter in a callee-saved
// register. Used right after the chain finishes: the host raises the DMA0
// line as soon as CS.INT is latched and re-asserts it until the guest
// clears INT, so this delay lets the delivery fire (and the handler run)
// while the guest is in a state that survives the x0-x18 clobber. The
// foreground code below only runs after CS.INT has been cleared.
#[no_mangle]
#[unsafe(naked)]
pub extern "C" fn delay_spin(_iterations: u64) {
    core::arch::naked_asm!(
        "stp x19, x20, [sp, #-16]!",
        "mov x19, x0",
        "1:",
        "subs x19, x19, #1",
        "b.ne 1b",
        "ldp x19, x20, [sp], #16",
        "ret"
    )
}

fn mem_eq(a: u32, b: u32, n: usize) -> bool {
    for i in 0..n {
        if unsafe { *(a as *const u8).add(i) } != unsafe { *(b as *const u8).add(i) } {
            return false;
        }
    }
    true
}

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    let mut ok = true;
    puts("dma: BCM2835 DMA controller @ 0x3F007000\r\n");

    // Source pattern: 0x5a + i.
    for i in 0..64u32 {
        unsafe { *((SRC + i) as *mut u8) = (0x5a + i) as u8 };
    }
    unsafe { *((FILL_BYTE) as *mut u8) = 0x77 };

    // Control block chain (32 bytes each, guest RAM; real layout: TI at +0,
    // SRC +4, DEST +8, TXFR_LEN +12, STRIDE +16, NEXTCONBK +20).
    unsafe {
        let c0 = CB0 as *mut u32;
        *c0 = TI_COPY;
        *c0.add(1) = SRC;
        *c0.add(2) = DST;
        *c0.add(3) = 64; // TXFR_LEN
        *c0.add(4) = 0; // STRIDE
        *c0.add(5) = CB1; // NEXTCONBK
        let c1 = CB1 as *mut u32;
        *c1 = TI_COPY;
        *c1.add(1) = DST;
        *c1.add(2) = DST2;
        *c1.add(3) = 32;
        *c1.add(4) = 0;
        *c1.add(5) = CB2;
        let c2 = CB2 as *mut u32;
        *c2 = TI_FILL;
        *c2.add(1) = FILL_BYTE;
        *c2.add(2) = DST3;
        *c2.add(3) = 16;
        *c2.add(4) = 0;
        *c2.add(5) = 0; // end of chain
    }

    // Enable DMA channel 0, arm the DMA0 IRQ in the interrupt controller,
    // install the vector table (same 0x100000 VBAR as the other guests).
    mmio_write(DMA_ENABLE, 1);
    mmio_write(IC_ENABLE_IRQS1, IRQ_DMA0);
    unsafe {
        core::arch::asm!(
            "mov x0, #0x100000",
            "msr vbar_el1, x0",
            "msr daifclr, #2",
            out("x0") _,
            options(nostack)
        );
    }
    puts("dma: channel 0 enabled, IRQ 16 armed, chain at 0x284000\r\n");
    mmio_write(DMA_CONBLK, CB0);
    mmio_write(DMA_CS, 1); // ACTIVE: host performs the chain between slices

    let done = poll_end(DMA_CS) == 1;
    // Let the completion IRQ fire (and be handled) while we're still in
    // register-independent spins, then clear CS.INT so no further deliveries
    // land in the middle of the foreground code below.
    delay_spin(1000);
    mmio_write(DMA_CS, 0); // clear INT (host write-mask semantics)
    puts(if done {
        "dma: chain done (END set)\r\n"
    } else {
        "dma: chain timeout\r\n"
    });
    ok &= done;

    // Verify the destinations.
    let mut pat = [0u8; 64];
    for i in 0..64 {
        pat[i] = (0x5a + i as u8) as u8;
    }
    let mut a = true;
    for (i, b) in pat.iter().enumerate() {
        if unsafe { *((DST + i as u32) as *const u8) } != *b {
            a = false;
        }
    }
    puts(if a {
        "dma: full copy OK (64 bytes)\r\n"
    } else {
        "dma: full copy FAIL\r\n"
    });
    ok &= a;

    let b = mem_eq(DST2, DST, 32);
    puts(if b {
        "dma: relay copy OK (32 bytes)\r\n"
    } else {
        "dma: relay copy FAIL\r\n"
    });
    ok &= b;

    let mut f = true;
    for i in 0..16u32 {
        if unsafe { *((DST3 + i) as *const u8) } != 0x77 {
            f = false;
        }
    }
    puts(if f {
        "dma: fill OK (SRC_IGNORE, 16 bytes)\r\n"
    } else {
        "dma: fill FAIL\r\n"
    });
    ok &= f;

    puts(if ok {
        "dma: all checks passed\r\n"
    } else {
        "dma: FAILED\r\n"
    });
    puts("dma: parked\r\n");
    mmio_write(DMA_DONE, 1);

    loop {
        core::hint::spin_loop();
    }
}
