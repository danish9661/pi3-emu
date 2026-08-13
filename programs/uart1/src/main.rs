#![no_std]
#![no_main]

//! Mini UART (UART1) demo: the BCM2835 AUX mini UART at 0x3F215000. The
//! guest configures it like a real driver (enable AUX, 8N1 via LCR, TX
//! enable via CNTL) and writes a diagnostic stream, which the host pumps
//! to the terminal tagged "[u1] " (src/uart1.js). UART0 stays the primary
//! console. TX flow control uses LSR bit 5 (TX empty), which the host
//! refreshes every slice.

use pi_runtime::puts;

const AUX: u32 = 0x3F21_5000;
const ENABLES: u32 = AUX + 0x04;
const IO: u32 = AUX + 0x40;
const LCR: u32 = AUX + 0x4C;
const LSR: u32 = AUX + 0x54;
const CNTL: u32 = AUX + 0x60;
const BAUD: u32 = AUX + 0x68;

const LSR_TX_EMPTY: u32 = 1 << 5;

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

// UART1 putc: byte-wide IO register with the same pulse protocol as UART0
// (wait for the host to clear the slot, then write) plus an LSR TX-empty
// check like a real mini-UART driver.
fn putc1(c: u8) {
    let mut spins = 0;
    while mmio_read(LSR) & LSR_TX_EMPTY == 0 {
        spins += 1;
        if spins > 0x40000 {
            break;
        }
    }
    spins = 0;
    while mmio_read(IO) != 0 {
        spins += 1;
        if spins > 0x40000 {
            break;
        }
    }
    mmio_write(IO, c as u32);
}

fn puts1(s: &str) {
    for b in s.bytes() {
        putc1(b);
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
    puts("uart1: mini UART (AUX) @ 0x3F215000\r\n");

    // Configure the mini UART like a real driver: enable the AUX peripheral,
    // 8N1, enable TX, 115200-ish.
    mmio_write(ENABLES, 1); // AUX_ENABLES: bit 0 = mini UART
    mmio_write(LCR, 3); // 8N1
    mmio_write(CNTL, 1); // TX enable
    mmio_write(BAUD, 270); // 250 MHz / (8 * 271) ~= 115200

    puts("uart1: UART0 console is active\r\n");
    puts("uart1: starting UART1 diagnostics\r\n");

    puts1("uart1: hello from the mini UART\r\n");
    puts1("uart1: LSR TX-empty pacing works\r\n");
    puts1("uart1: diag line 3/3\r\n");

    puts("uart1: UART1 diagnostics complete\r\n");
    puts("uart1: parked\r\n");

    loop {
        core::hint::spin_loop();
    }
}
