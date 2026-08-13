#![no_std]
#![no_main]

//! BCM2837 system-timer demo: the free-running 40-bit counter (CLO/CHI,
//! 1 us ticks of host wall clock, epoch reset at boot) drives a 1-second
//! sleep, and the C1 compare register demonstrates the CS match bits.
//!
//! Timer window (0x3F003000 — real BCM2837 layout, host-refreshed):
//!   +0x00 CS    match flags M0..M3 (host-set, guest clears W1C)
//!   +0x04 CLO   counter low 32 bits (host: us since program boot)
//!   +0x08 CHI   counter high 32 bits
//!   +0x0C C0    compare 0
//!   +0x10 C1    compare 1
//!   +0x14 C2    compare 2
//!   +0x18 C3    compare 3
//!   +0x20 DONE  host extension: guest writes 1 when finished

use pi_runtime::{puts, putu};

const TMR: u32 = 0x3F00_3000;
const TMR_CS: u32 = TMR + 0x00;
const TMR_CLO: u32 = TMR + 0x04;
// +0x08 CHI: counter high bits (host-refreshed; demo uses only CLO)
const TMR_C1: u32 = TMR + 0x10;
const TMR_DONE: u32 = TMR + 0x20;

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

/// Busy-wait for `us` microseconds of timer ticks.
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
    let t0 = now_us();
    puts("boot: CLO = ");
    putu(t0 as u64);
    puts(" us\r\n");

    // compare register: interrupt-free demo — arm C1 for +0.5 s, then poll
    // the M1 match bit in CS (the host sets it once CLO passes C1).
    mmio_write(TMR_C1, t0.wrapping_add(500_000));
    puts("armed C1 = ");
    putu(t0.wrapping_add(500_000) as u64);
    puts(" us\r\n");

    puts("sleeping 1 s...\r\n");
    sleep_us(1_000_000);

    let t1 = now_us();
    puts("woke: CLO = ");
    putu(t1 as u64);
    puts(" us (elapsed ");
    putu((t1.wrapping_sub(t0)) as u64);
    puts(")\r\n");

    let m1 = (mmio_read(TMR_CS) >> 1) & 1;
    puts("C1 match (M1) = ");
    putu(m1 as u64);
    puts("\r\n");
    puts("clearing M1\r\n");
    // CS is write-mask here (host-arbitrated memory can't observe W1C writes
    // that don't change bytes): rewrite the status with M1 removed.
    mmio_write(TMR_CS, mmio_read(TMR_CS) & !(1 << 1));
    sleep_us(10_000); // cross several arbitration points so the host applies the clear
    puts("CS after clear = ");
    putu((mmio_read(TMR_CS) & 0xF) as u64);
    puts("\r\ndone\r\n");

    mmio_write(TMR_DONE, 1);
    loop {}
}