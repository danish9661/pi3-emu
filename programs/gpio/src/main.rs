#![no_std]
#![no_main]

//! BCM2837 GPIO demo: 8 LEDs on pins 21..28 run a knight-rider chase timed
//! by the system timer, then a pull-down button on pin 29 is polled, and
//! finally the rising-edge IRQ path is exercised: GPREN0 bit 29 arms the
//! GPIO bank-0 IRQ (legacy IC bank-2 bit 17 = IRQ 81), the host's next
//! button press fires the vector and the handler acks GPEDS (W1C).
//!
//! GPIO window (0x3F200000 — real BCM2837 layout, host-arbitrated):
//!   +0x08 GPFSEL2   function select for pins 20..29 (LEDs + button)
//!   +0x1C GPSET0    set pins 0..31 high
//!   +0x28 GPCLR0    set pins 0..31 low
//!   +0x34 GPLEV0    level read (the host refreshes input pins)
//!   +0x40 GPEDS0    event detect status (write-1-to-clear)
//!   +0x4C GPREN0    rising-edge detect enable
//!   +0x94 GPPUD     pull-up/down control
//!   +0x98 GPPUDCLK0 pull clock strobe for pins 0..31
//!
//! Timer window (0x3F003000): CLO is the host wall clock in us; +0x20 DONE
//! is the host extension that parks the guest after the chase.

use pi_runtime::{puts, putx};

const GPIO_BASE: u32 = 0x3F20_0000;
const GPFSEL0: u32 = GPIO_BASE + 0x00;
const GPSET0: u32 = GPIO_BASE + 0x1C;
const GPCLR0: u32 = GPIO_BASE + 0x28;
const GPLEV0: u32 = GPIO_BASE + 0x34;
const GPEDS0: u32 = GPIO_BASE + 0x40;
const GPREN0: u32 = GPIO_BASE + 0x4C;
const GPPUD: u32 = GPIO_BASE + 0x94;
const GPPUDCLK0: u32 = GPIO_BASE + 0x98;

const IC_BASE: u32 = 0x3F00_B200;
const IC_PENDING2: u32 = IC_BASE + 0x08;
const IC_ENABLE_IRQS2: u32 = IC_BASE + 0x14;
const IRQ_GPIO0: u32 = 1 << 17; // GPIO bank 0 (pins 0-31) = IRQ 81

const TMR_CLO: u32 = 0x3F00_3004;
const TMR_DONE: u32 = 0x3F00_3020;

const LED0: u32 = 21;
const LEDS: u32 = 8;
const BTN: u32 = 29;

static mut IRQ_SEEN: u32 = 0;

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

fn now_us() -> u32 {
    mmio_read(TMR_CLO)
}

/// Busy-wait for `us` microseconds of timer ticks (same trick as clock).
fn sleep_us(us: u32) {
    let dl = now_us().wrapping_add(us);
    // (now - dl) has bit 31 set while now < dl: keep spinning until passed
    while now_us().wrapping_sub(dl) & 0x8000_0000 != 0 {
        core::hint::spin_loop();
    }
}

// Vector table + host-assisted IRQ delivery glue (same scheme as the irq
// guest: the host starts the slice at VBAR+0x280, the glue runs the handler
// preserving the full register file — the guest may be interrupted mid-puts —
// then signals IC_IRQ_RET (0x3F00B22C) so the host resumes the guest).
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
    let p2 = mmio_read(IC_PENDING2);
    if p2 & IRQ_GPIO0 != 0 {
        let ev = mmio_read(GPEDS0) & (1 << BTN);
        if ev != 0 {
            puts("gpio: IRQ on BTN 29 (GPEDS 0x");
            putx(ev as u64);
            puts(")\r\n");
            mmio_write(GPEDS0, 1 << BTN); // W1C: drops the line
            unsafe { IRQ_SEEN = 1; }
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
    puts("gpio: LEDs 21..28 output, BTN 29 input (pull-down)\r\n");

    // outputs for 21..28, input for 29 — all pins live in GPFSEL2
    let mut fsel = 0u32;
    for p in LED0..LED0 + LEDS {
        fsel |= 0b001 << ((p - 20) * 3);
    }
    mmio_write(GPFSEL0 + 8, fsel);

    // pull-down on the button: GPPUD=1, strobe GPPUDCLK0 bit 29, then release
    mmio_write(GPPUD, 1);
    sleep_us(2);
    mmio_write(GPPUDCLK0, 1 << BTN);
    sleep_us(2);
    mmio_write(GPPUD, 0);
    mmio_write(GPPUDCLK0, 0);

    // knight-rider chase: bounce across the 8 LEDs, dot per pass
    puts("chase:\r\n");
    let mut idx = 0u32;
    let mut dir = 1i32;
    let mut pass = 0u32;
    while pass < 3 {
        mmio_write(GPSET0, 1 << (LED0 + idx));
        sleep_us(40_000);
        mmio_write(GPCLR0, 1 << (LED0 + idx));
        if dir > 0 && idx == LEDS - 1 {
            dir = -1;
            pass += 1;
            puts(".");
        } else if dir < 0 && idx == 0 {
            dir = 1;
            pass += 1;
            puts(".");
        } else {
            idx = (idx as i32 + dir) as u32;
        }
    }
    puts("\r\nchase done - polling BTN 29\r\n");
    mmio_write(TMR_DONE, 1);

    // Poll for the first press (the probe raises the button), then switch
    // to the IRQ path: arm the rising-edge detect + the GPIO bank-0 line.
    while mmio_read(GPLEV0) & (1 << BTN) == 0 {
        core::hint::spin_loop();
    }
    puts("button: 1 pressed\r\n");

    unsafe {
        core::arch::asm!(
            "mov x0, #0x100000",
            "msr vbar_el1, x0",
            "msr daifclr, #2",
            out("x0") _,
            options(nostack)
        );
        mmio_write(GPREN0, 1 << BTN); // rising-edge detect on BTN 29
        mmio_write(IC_ENABLE_IRQS2, IRQ_GPIO0); // GPIO bank 0 -> IRQ 81
        puts("gpio: GPREN armed on BTN 29 -> IRQ 81 — press the button\r\n");
        while IRQ_SEEN == 0 {
            core::arch::asm!("msr daifclr, #2", options(nostack));
            core::hint::spin_loop();
        }
        puts("gpio: IRQ phase done\r\n");
        loop {
            core::arch::asm!("msr daifclr, #2", options(nostack));
            core::hint::spin_loop();
        }
    }
}