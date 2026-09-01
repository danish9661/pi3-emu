#![no_std]
#![no_main]

//! Debug/diagnostic guest: exercises all BCM2835 peripherals at their real
//! offsets and prints a detailed status report. Useful for regression
//! testing and verifying the M30 device models.

use pi_runtime::{puts, putx, putu};

const GPIO_BASE: u32 = 0x3F20_0000;
const UART0_BASE: u32 = 0x3F20_1000;
const SPI0_BASE: u32 = 0x3F20_4000;
const PWM_BASE: u32 = 0x3F20_C000;
const I2S_BASE: u32 = 0x3F20_3000;
const TMR_BASE: u32 = 0x3F00_3000;
const MBOX_BASE: u32 = 0x3F00_B880;
const IC_BASE: u32 = 0x3F00_B200;
const SD_BASE: u32 = 0x3F30_0000;
const LOCAL_BASE: u32 = 0x4000_0000;
const RNG_BASE: u32 = 0x3F10_4000;
const CLK_BASE: u32 = 0x3F10_0000;
const SPI1_BASE: u32 = 0x3F21_5000;
const USB_BASE: u32 = 0x3F98_0000;
const UART2_BASE: u32 = 0x3F21_6000;
const UART3_BASE: u32 = 0x3F21_7000;
const UART4_BASE: u32 = 0x3F21_8000;
const UART5_BASE: u32 = 0x3F21_9000;
const DMA_BASE: u32 = 0x3F00_7000;
const DONE_REG: u32 = 0x3F98_0054;

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

fn check(name: &str, addr: u32, mask: u32, expect: u32) -> bool {
    let v = mmio_read(addr);
    let ok = (v & mask) == expect;
    puts("  ");
    puts(name);
    puts(": 0x");
    putx(v as u64);
    if ok {
        puts(" [OK]\r\n");
    } else {
        puts(" [FAIL want 0x");
        putx(expect as u64);
        puts("]\r\n");
    }
    ok
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
    let mut pass = 0u32;
    let mut fail = 0u32;

    puts("=== pi3-emu debug/diagnostic ===\r\n\r\n");

    // System timer
    puts("[System Timer]\r\n");
    if check("CLO", TMR_BASE + 0x04, 0xFFFF_FFFF, 0) {
        pass += 1;
    } else { fail += 1; }

    // GPIO
    puts("\r\n[GPIO]\r\n");
    if check("GPFSEL0", GPIO_BASE, 0xFFFF_FFFF, 0) {
        pass += 1;
    } else { fail += 1; }
    if check("GPLEV0", GPIO_BASE + 0x34, 0xFFF8_0000, 0) {
        pass += 1;
    } else { fail += 1; }

    // PL011 UART0
    puts("\r\n[UART0 (PL011)]\r\n");
    if check("FR", UART0_BASE + 0x18, 0xFF, 0) {
        pass += 1;
    } else { fail += 1; }

    // Interrupt controller
    puts("\r\n[Legacy IC]\r\n");
    if check("IC_BASIC", IC_BASE, 0xFFF0_0000, 0) {
        pass += 1;
    } else { fail += 1; }

    // VideoCore mailbox
    puts("\r\n[VideoCore Mailbox]\r\n");
    if check("STATUS", MBOX_BASE + 0x18, 0x8000_0000, 0x8000_0000) {
        pass += 1;
    } else { fail += 1; }

    // RNG
    puts("\r\n[RNG]\r\n");
    if check("CTRL", RNG_BASE, 0x0000_000F, 0) {
        pass += 1;
    } else { fail += 1; }
    let rng_data = mmio_read(RNG_BASE + 0x04);
    puts("  DATA: 0x");
    putx(rng_data as u64);
    if rng_data != 0 {
        puts(" [OK]\r\n");
        pass += 1;
    } else {
        puts(" [WARN zero]\r\n");
        fail += 1;
    }

    // Temperature
    puts("\r\n[Temperature]\r\n");
    let temp = mmio_read(RNG_BASE + 0x04);
    puts("  Millidegrees: ");
    putu(temp as u64);
    if temp >= 30000 && temp <= 60000 {
        puts(" [OK]\r\n");
        pass += 1;
    } else {
        puts(" [WARN out of range]\r\n");
        fail += 1;
    }

    // Clock Manager
    puts("\r\n[Clock Manager]\r\n");
    if check("PWMCLK", CLK_BASE + 0xA0, 0x001F_F000, 0) {
        pass += 1;
    } else { fail += 1; }

    // I2S
    puts("\r\n[I2S (PCM)]\r\n");
    if check("CS_A", I2S_BASE, 0x00FF_FFFF, 0) {
        pass += 1;
    } else { fail += 1; }

    // SPI0
    puts("\r\n[SPI0]\r\n");
    if check("CS", SPI0_BASE, 0xFFFF_FFFF, 0) {
        pass += 1;
    } else { fail += 1; }

    // PWM
    puts("\r\n[PWM]\r\n");
    if check("CTL", PWM_BASE, 0xFF, 0) {
        pass += 1;
    } else { fail += 1; }

    // SDHCI
    puts("\r\n[SDHCI (EMMC)]\r\n");
    if check("INTERRUPT", SD_BASE + 0x30, 0xFFFF_FFFF, 0) {
        pass += 1;
    } else { fail += 1; }

    // SPI1 (AUX)
    puts("\r\n[SPI1 (AUX)]\r\n");
    if check("AUX_IRQ", SPI1_BASE, 0x01, 0) {
        pass += 1;
    } else { fail += 1; }

    // USB DWC2
    puts("\r\n[USB (DWC2)]\r\n");
    let snpsid = mmio_read(USB_BASE + 0x40);
    puts("  GSNPSID: 0x");
    putx(snpsid as u64);
    if snpsid == 0x4F54_280A {
        puts(" [OK]\r\n");
        pass += 1;
    } else {
        puts(" [FAIL]\r\n");
        fail += 1;
    }

    // UART2-5
    puts("\r\n[UART2-5 (Mini)]\r\n");
    let bases = [UART2_BASE, UART3_BASE, UART4_BASE, UART5_BASE];
    for b in bases {
        let lsr = mmio_read(b + 0x54);
        puts("  UART@0x");
        putx(b as u64);
        puts(" LSR=0x");
        putx(lsr as u64);
        if lsr <= 0xFF {
            puts(" [OK]\r\n");
            pass += 1;
        } else {
            puts(" [FAIL]\r\n");
            fail += 1;
        }
    }

    // DMA
    puts("\r\n[DMA]\r\n");
    if check("CS", DMA_BASE, 0xFFFF_FFFF, 0) {
        pass += 1;
    } else { fail += 1; }

    // Local interrupt block
    puts("\r\n[Local IC]\r\n");
    if check("CONTROL", LOCAL_BASE, 0xFFFF_FFFF, 0) {
        pass += 1;
    } else { fail += 1; }

    // Summary
    puts("\r\n=== Summary ===\r\n");
    puts("  Pass: ");
    putu(pass as u64);
    puts("\r\n  Fail: ");
    putu(fail as u64);
    puts("\r\n  Total: ");
    putu((pass + fail) as u64);
    puts("\r\n");
    if fail == 0 {
        puts("  ALL PASS\r\n");
    } else {
        puts("  SOME FAILURES\r\n");
    }

    mmio_write(DONE_REG, 1);
    loop { core::hint::spin_loop(); }
}
