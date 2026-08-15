#![no_std]
#![no_main]

//! PL011 UART0 demo: the BCM2837 primary UART at 0x3F201000, driven like
//! real hardware. The guest writes the baud divisors and line control,
//! enables UARTEN|TXE|RXE, verifies the configuration read-back, prints
//! with FR.TXFF pacing (never full in this model), then arms RXINTR
//! (IMSC bit 4) — which drives the real PL011 IRQ line 57 (bank 2, bit
//! 25) — and echoes every key the host sends, reporting MIS.
//!
//! Phase 3: TXIM (IMSC bit 5). The TX FIFO always has room in this model,
//! so arming TXIM asserts the line immediately — the handler reads MIS,
//! recognizes TXINTR, and de-arms TXIM so the line drops (a real driver's
//! TX-empty drain). No storm: after the de-arm the IRQ line stays low.
//!
//! Vector table lives in the `.vectors` section at 0x100000 (linker.ld).

use pi_runtime::{putc, puts, putx};

const UART0: u32 = 0x3F20_1000;
const DR: u32 = UART0 + 0x00;
const FR: u32 = UART0 + 0x18;
const IBRD: u32 = UART0 + 0x24;
const FBRD: u32 = UART0 + 0x28;
const LCRH: u32 = UART0 + 0x2C;
const CR: u32 = UART0 + 0x30;
const IMSC: u32 = UART0 + 0x38;
const MIS: u32 = UART0 + 0x40;
const ICR: u32 = UART0 + 0x44;

const IC_BASE: u32 = 0x3F00_B200;
const IC_PENDING2: u32 = IC_BASE + 0x08;
const IC_ENABLE_IRQS2: u32 = IC_BASE + 0x14;

const IRQ_UART: u32 = 1 << 25; // PL011 UART0 = IRQ 57 (bank 2, bit 25)

const FR_RXFE: u32 = 1 << 4;
const RXIM: u32 = 1 << 4; // IMSC/MIS bit 4
const TXIM: u32 = 1 << 5; // IMSC/MIS bit 5

static mut PHASE: u32 = 0; // 0 = RX phase, 1 = TXIM phase, 2 = done

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

// Vector table + entry glue (same host-assisted delivery as the irq
// program: the host starts the slice at VBAR+0x280, irq_glue runs the
// handler, then signals IC_IRQ_RET so the host resumes the guest).
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
    "  stp x29, x30, [sp, #-16]!", // preserve the guest's LR: the bl below
    "  bl irq_handler_rust", //      clobbers x30 and the resumed code must
    "  ldp x29, x30, [sp], #16", // not `ret` into the glue
    "  movz w0, #1",
    "  movz w1, #0xb22c",
    "  movk w1, #0x3f00, lsl #16",
    "  str w0, [x1]",
    "  b .",
);

#[no_mangle]
pub extern "C" fn irq_handler_rust() {
    let p2 = mmio_read(IC_PENDING2);
    if p2 & IRQ_UART != 0 {
        let mis = mmio_read(MIS);
        if mis & TXIM != 0 {
            puts("uart0: TXINTR fired (MIS 0x");
            putx(mis as u64);
            puts(")\r\n");
            mmio_write(IMSC, RXIM); // de-arm TXIM: the line drops, no storm
            puts("uart0: TXIM de-armed\r\n");
            unsafe { PHASE = 2; }
        }
        if mis & RXIM != 0 {
            puts("uart0: RXINTR fired (MIS 0x");
            putx(mis as u64);
            puts(")\r\n");
            while mmio_read(FR) & FR_RXFE == 0 {
                let ch = (mmio_read(DR) & 0xff) as u8; // reading DR pops the FIFO
                puts("uart0: [rx '");
                putc(ch);
                puts("']\r\n");
            }
            mmio_write(ICR, RXIM); // W1C: absorb (the line follows the FIFO)
            unsafe { PHASE = 1; }
        }
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
    puts("uart0: BCM2837 PL011 @ 0x3F201000\r\n");

    // Configure like a real driver: 115200 baud @ 3 MHz, 8N1, FIFO on.
    mmio_write(IBRD, 1);
    mmio_write(FBRD, 40);
    mmio_write(LCRH, (3 << 5) | (1 << 4)); // WLEN 11 (8-bit) | FEN
    mmio_write(CR, 1 | (1 << 8) | (1 << 9)); // UARTEN | TXE | RXE

    // Read the configuration back through the window.
    let ibrd = mmio_read(IBRD);
    let fbrd = mmio_read(FBRD);
    let lcrh = mmio_read(LCRH);
    let cr = mmio_read(CR);
    puts("uart0: IBRD ");
    putx(ibrd as u64);
    puts(" FBRD ");
    putx(fbrd as u64);
    puts(" (115200 @ 3 MHz), LCRH 0x");
    putx(lcrh as u64);
    puts(", CR 0x");
    putx(cr as u64);
    puts("\r\n");

    // TX with FR.TXFF pacing (the host always shows room, TXFE set).
    puts("uart0: FR shows TXFE set, TXFF clear — TX ready\r\n");

    unsafe {
        core::arch::asm!(
            "mov x0, #0x100000",
            "msr vbar_el1, x0",
            "msr daifclr, #2",
            out("x0") _,
            options(nostack)
        );
        mmio_write(IMSC, RXIM); // arm RXINTR
        mmio_write(IC_ENABLE_IRQS2, IRQ_UART); // enable line 57 (bank 2)
        puts("uart0: RXINTR armed (IMSC bit 4) -> IRQ 57 — type a key\r\n");
        while PHASE != 1 {
            core::arch::asm!("msr daifclr, #2", options(nostack));
            core::hint::spin_loop();
        }
        // Phase 3: arm TXIM — the TX FIFO always has room, so the line
        // asserts immediately and the handler de-arms it.
        mmio_write(IMSC, RXIM | TXIM);
        puts("uart0: TXIM armed (IMSC bit 5)\r\n");
        while PHASE != 2 {
            core::arch::asm!("msr daifclr, #2", options(nostack));
            core::hint::spin_loop();
        }
        puts("uart0: TX phase done - no IRQ storm after the de-arm\r\n");
        loop {
            core::arch::asm!("msr daifclr, #2", options(nostack));
            core::hint::spin_loop();
        }
    }
}
