#![no_std]

//! Minimal bare-metal runtime for guest programs on the pi3-emu UART.
//!
//! Device window (0x3F201000, 4 KiB):
//!   +0x00   TX slot 0 — host drains one char per slice (pulse protocol:
//!           putc writes a char, then spins until the host clears it)
//!   +0x80   RX slot  — host writes a byte, getc consumes it

use core::panic::PanicInfo;

const UART: *mut u32 = 0x3F20_1000 as *mut u32;
const RX: *mut u32 = (0x3F20_1000 + 0x80) as *mut u32;

#[inline]
pub fn putc(c: u8) {
    unsafe {
        while *UART != 0 {
            core::hint::spin_loop();
        }
        *UART = c as u32;
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

/// Block until a key is available, then return it (consuming the slot).
pub fn getc() -> u8 {
    unsafe {
        loop {
            let c = *RX;
            if c != 0 {
                *RX = 0;
                return c as u8;
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
    unsafe {
        while *UART != 0 {
            core::hint::spin_loop();
        }
        *UART = b'!' as u32;
    }
    loop {}
}
