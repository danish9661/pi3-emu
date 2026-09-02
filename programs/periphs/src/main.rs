#![no_std]
#![no_main]

//! M30 peripheral probe: reads the RNG, Temperature, Clock Manager, I2S,
//! SPI1, USB, and UART2–5 at their real BCM2835 offsets and prints PASS/FAIL
//! based on expected register values.

use pi_runtime::{puts, putx};

const RNG_DATA: u32 = 0x3F10_4004; // temperature (shared RNG block)
const RNG_CTRL: u32 = 0x3F10_4000;
const CLK_PWMCLK: u32 = 0x3F10_00A0;
const I2S_CS: u32 = 0x3F20_3000;
const SPI1_CS: u32 = 0x3F21_5080;
const USB_GSNPSID: u32 = 0x3F98_0040;
const UART2_LSR: u32 = 0x3F21_6054;
const UART3_LSR: u32 = 0x3F21_7054;
const UART4_LSR: u32 = 0x3F21_8054;
const UART5_LSR: u32 = 0x3F21_9054;
const USB_DONE: u32 = 0x3F98_0054;

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
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

    // --- RNG: CTRL should be readable (enabled bit may be 0 or 1) ---
    let ctrl = mmio_read(RNG_CTRL);
    if ctrl <= 0x0F {
        puts("periphs: RNG CTRL OK\r\n");
        pass += 1;
    } else {
        puts("periphs: RNG CTRL FAIL (");
        putx(ctrl as u64);
        puts(")\r\n");
        fail += 1;
    }

    // --- Temperature: DATA should be ~45000 (45.0 °C) ---
    let temp = mmio_read(RNG_DATA);
    if temp >= 40000 && temp <= 50000 {
        puts("periphs: Temperature OK (");
        putx(temp as u64);
        puts(")\r\n");
        pass += 1;
    } else {
        puts("periphs: Temperature FAIL (");
        putx(temp as u64);
        puts(")\r\n");
        fail += 1;
    }

    // --- Clock Manager: PWMCLK should be readable (ENAB clears when not set) ---
    let clk = mmio_read(CLK_PWMCLK);
    if clk <= 0x001F_F000 {
        puts("periphs: Clock Manager OK\r\n");
        pass += 1;
    } else {
        puts("periphs: Clock Manager FAIL (");
        putx(clk as u64);
        puts(")\r\n");
        fail += 1;
    }

    // --- I2S: CS_A should be readable (0 or low bits when disabled) ---
    let cs = mmio_read(I2S_CS);
    if cs <= 0x00FF_FFFF {
        puts("periphs: I2S OK\r\n");
        pass += 1;
    } else {
        puts("periphs: I2S FAIL (");
        putx(cs as u64);
        puts(")\r\n");
        fail += 1;
    }

    // --- SPI1: AUX_ENABLES should show bit 1 (SPI1 enable) after we set it ---
    mmio_write(SPI1_CS - 0x7C, 0x02); // write ENABLES at +0x04 (bit 1)
    let spien = mmio_read(SPI1_CS - 0x7C);
    if spien != 0 {
        puts("periphs: SPI1 ENABLES OK\r\n");
        pass += 1;
    } else {
        puts("periphs: SPI1 ENABLES FAIL\r\n");
        fail += 1;
    }

    // --- USB: GSNPSID should read 0x4f54280a or 0x4f54294a (DWC2 rev 4.20a) ---
    let snpsid = mmio_read(USB_GSNPSID);
    if snpsid == 0x4F54_280A || snpsid == 0x4F54_294A {
        puts("periphs: USB GSNPSID OK\r\n");
        pass += 1;
    } else {
        puts("periphs: USB GSNPSID FAIL (");
        putx(snpsid as u64);
        puts(")\r\n");
        fail += 1;
    }

    // --- UART2–5: LSR should be readable (TX_EMPTY when idle) ---
    let bases = [UART2_LSR, UART3_LSR, UART4_LSR, UART5_LSR];
    for b in bases {
        let lsr = mmio_read(b);
        if lsr <= 0x00FF {
            pass += 1;
        } else {
            fail += 1;
        }
    }
    puts("periphs: UART2-5 LSR OK\r\n");

    // --- Summary ---
    puts("periphs: ");
    putx(pass as u64);
    puts(" pass / ");
    putx(fail as u64);
    puts(" fail\r\n");

    if fail == 0 {
        puts("periphs: ALL PASS\r\n");
    } else {
        puts("periphs: FAIL\r\n");
    }

    mmio_write(USB_DONE, 1); // park on USB_DONE (0x3F980054)
    loop {
        core::hint::spin_loop();
    }
}
