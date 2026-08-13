#![no_std]
#![no_main]

//! BCM2837 GPIO demo: 8 LEDs on pins 21..28 run a knight-rider chase timed
//! by the system timer, then a pull-down button on pin 29 is polled.
//!
//! GPIO window (0x3F200000 — real BCM2837 layout, host-arbitrated):
//!   +0x08 GPFSEL2   function select for pins 20..29 (LEDs + button)
//!   +0x1C GPSET0    set pins 0..31 high
//!   +0x28 GPCLR0    set pins 0..31 low
//!   +0x34 GPLEV0    level read (the host refreshes input pins)
//!   +0x94 GPPUD     pull-up/down control
//!   +0x98 GPPUDCLK0 pull clock strobe for pins 0..31
//!
//! Timer window (0x3F003000): CLO is the host wall clock in us; +0x20 DONE
//! is the host extension that parks the guest after the chase.

use pi_runtime::{puts, putu};

const GPIO_BASE: u32 = 0x3F20_0000;
const GPFSEL0: u32 = GPIO_BASE + 0x00;
const GPSET0: u32 = GPIO_BASE + 0x1C;
const GPCLR0: u32 = GPIO_BASE + 0x28;
const GPLEV0: u32 = GPIO_BASE + 0x34;
const GPPUD: u32 = GPIO_BASE + 0x94;
const GPPUDCLK0: u32 = GPIO_BASE + 0x98;

const TMR_CLO: u32 = 0x3F00_3004;
const TMR_DONE: u32 = 0x3F00_3020;

const LED0: u32 = 21;
const LEDS: u32 = 8;
const BTN: u32 = 29;

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
    puts("\r\nchase done - hold BTN 29\r\n");
    mmio_write(TMR_DONE, 1);

    // poll the button; report each press edge
    let mut presses = 0u32;
    let mut prev = false;
    loop {
        let b = mmio_read(GPLEV0) & (1 << BTN) != 0;
        if b && !prev {
            presses += 1;
            puts("button: ");
            putu(presses as u64);
            puts(" pressed\r\n");
        }
        prev = b;
        core::hint::spin_loop();
    }
}