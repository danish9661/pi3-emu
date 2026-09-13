#![no_std]
#![no_main]

//! M52 kernel spike: the first step of the own-Rust-kernel track — a
//! minimal kernel loaded at the real Pi boot address 0x80000 that prints
//! to the PL011 and echoes input (the 05_drivers_gpio_uart milestone
//! shape: "Echoing input now"). Only proven pi-cpu instructions are used
//! (plain loads/stores, branches, UART MMIO); no EL juggling, no MMU, no
//! timer yet — those are the M52-stretch items in AGENTS.md.

use core::panic::PanicInfo;

const UART0: u32 = 0x3F20_1000;
const DR: u32 = UART0 + 0x00;
const FR: u32 = UART0 + 0x18;
const IBRD: u32 = UART0 + 0x24;
const FBRD: u32 = UART0 + 0x28;
const LCRH: u32 = UART0 + 0x2C;
const CR: u32 = UART0 + 0x30;

const FR_TXFF: u32 = 1 << 5;
const FR_RXFE: u32 = 1 << 4;

unsafe extern "C" {
    static mut __bss_start: u64;
    static mut __bss_end: u64;
}

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

fn putc(c: u8) {
    unsafe {
        for _ in 0..2000 {
            if core::ptr::read_volatile((UART0 + 0x18) as *const u32) & FR_TXFF == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        core::ptr::write_volatile(UART0 as *mut u32, c as u32);
    }
}

fn puts(s: &str) {
    for b in s.bytes() {
        putc(b);
    }
}

fn getc() -> u8 {
    loop {
        if mmio_read(FR) & FR_RXFE == 0 {
            return (mmio_read(DR) & 0xff) as u8;
        }
        core::hint::spin_loop();
    }
}

#[no_mangle]
#[unsafe(naked)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        // Stack at the top of the 4 MB RAM, like the programs/ guests.
        "movz x9, #0xfff0",
        "movk x9, #0x3f, lsl #16",
        "mov sp, x9",
        // Zero .bss (x0-x2 boot args are already consumed: none taken).
        "adr x9, __bss_start",
        "adr x10, __bss_end",
        "0:",
        "cmp x9, x10",
        "b.hs 1f",
        "str xzr, [x9], #8",
        "b 0b",
        "1:",
        "b rust_main",
    )
}

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    // PL011 init: 115200 baud @ 3 MHz, 8N1, FIFO on, UARTEN|TXE|RXE.
    mmio_write(IBRD, 1);
    mmio_write(FBRD, 40);
    mmio_write(LCRH, (3 << 5) | (1 << 4));
    mmio_write(CR, 1 | (1 << 8) | (1 << 9));

    puts("rpi-kernel M52: hello from 0x80000\r\n");
    puts("rpi-kernel: PL011 ready, Echoing input now\r\n");
    loop {
        let c = getc();
        puts("rpi-kernel: [echo '");
        putc(c);
        puts("']\r\n");
    }
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    puts("rpi-kernel: PANIC\r\n");
    loop {
        core::hint::spin_loop();
    }
}
