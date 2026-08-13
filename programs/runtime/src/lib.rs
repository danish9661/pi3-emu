#![no_std]

//! Minimal bare-metal runtime for guest programs on the pi3-emu UART.
//!
//! The runtime configures the BCM2837 PL011 UART0 (0x3F201000) like a
//! real driver — IBRD/FBRD for 115200 baud at a 3 MHz UART clock
//! (1 + 40/64), 8N1 with the FIFO enabled, CR = UARTEN|TXE|RXE — lazily
//! on first use, then:
//!   putc  polls FR.TXFF (never full in this model), writes DR
//!   getc  polls FR.RXFE (host-refreshed), reads DR (pops the host FIFO)

use core::panic::PanicInfo;

const UART0: *mut u32 = 0x3F20_1000 as *mut u32;

const FR_TXFF: u32 = 1 << 5; // TX FIFO full
const FR_RXFE: u32 = 1 << 4; // RX FIFO empty
const CR_UARTEN: u32 = 1 << 0;
const CR_TXE: u32 = 1 << 8;
const CR_RXE: u32 = 1 << 9;
const LCRH_8N1_FEN: u32 = (3 << 5) | (1 << 4); // 8-bit, FIFO enabled
const IBRD_115200: u32 = 1; // 3 MHz / (16 * 115200) = 1.628...
const FBRD_115200: u32 = 40; // ...fraction 0.628 * 64 = 40.2

static mut UART_INIT: u32 = 0;

fn uart_init() {
    unsafe {
        if core::ptr::read_volatile(&raw const UART_INIT) != 0 {
            return;
        }
        core::ptr::write_volatile(&raw mut UART_INIT, 1);
        core::ptr::write_volatile(UART0.add(0x24 / 4), IBRD_115200); // IBRD
        core::ptr::write_volatile(UART0.add(0x28 / 4), FBRD_115200); // FBRD
        core::ptr::write_volatile(UART0.add(0x2C / 4), LCRH_8N1_FEN); // LCRH
        core::ptr::write_volatile(UART0.add(0x30 / 4), CR_UARTEN | CR_TXE | CR_RXE); // CR
    }
}

#[inline]
pub fn putc(c: u8) {
    uart_init();
    unsafe {
        for _ in 0..2000 {
            if core::ptr::read_volatile(UART0.add(0x18 / 4)) & FR_TXFF == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        core::ptr::write_volatile(UART0, c as u32); // DR
    }
}

pub fn puts(s: &str) {
    for b in s.bytes() {
        putc(b);
    }
}

/// Print an unsigned integer in decimal.
pub fn putu(mut n: u64) {
    if n == 0 {
        putc(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    while n > 0 {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    for &b in &buf[i..] {
        putc(b);
    }
}

/// Print an unsigned integer in lowercase hex (no leading 0x).
pub fn putx(mut n: u64) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = [0u8; 16];
    let mut i = buf.len();
    if n == 0 {
        putc(b'0');
        return;
    }
    while n > 0 {
        i -= 1;
        buf[i] = HEX[(n & 0xf) as usize];
        n >>= 4;
    }
    for &b in &buf[i..] {
        putc(b);
    }
}

/// Block until a key is available, then return it (reading DR pops the
/// host's RX FIFO — a second read without a key would return 0).
pub fn getc() -> u8 {
    uart_init();
    unsafe {
        loop {
            if core::ptr::read_volatile(UART0.add(0x18 / 4)) & FR_RXFE == 0 {
                return (core::ptr::read_volatile(UART0) & 0xff) as u8;
            }
            core::hint::spin_loop();
        }
    }
}

// Each guest program defines its own `_start` (kept first in .text via KEEP)
// which calls its `rust_main`. The runtime lib only provides UART I/O helpers
// and the panic handler.

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    uart_init();
    unsafe {
        for _ in 0..2000 {
            if core::ptr::read_volatile(UART0.add(0x18 / 4)) & FR_TXFF == 0 {
                break;
            }
            core::hint::spin_loop();
        }
        core::ptr::write_volatile(UART0, b'!' as u32);
    }
    loop {}
}
