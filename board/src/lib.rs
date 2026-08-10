#![no_std]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

pub const PI_UART_BASE: u32 = 0x3F20_1000;
pub const PI_UART_WINDOW: u32 = 0x1000;

const FIFO_LEN: usize = 256;
const EMPTY: u32 = 0xFFFF_FFFF;

struct Uart {
    fifo: UnsafeCell<[u8; FIFO_LEN]>,
    rd: AtomicUsize,
    wr: AtomicUsize,
}

unsafe impl Sync for Uart {}

static UART: Uart = Uart {
    fifo: UnsafeCell::new([0u8; FIFO_LEN]),
    rd: AtomicUsize::new(0),
    wr: AtomicUsize::new(0),
};

fn push(c: u8) {
    let u = &UART;
    let wr = u.wr.load(Ordering::Relaxed);
    let next = (wr + 1) % FIFO_LEN;
    if next != u.rd.load(Ordering::Relaxed) {
        let fifo = unsafe { &mut *u.fifo.get() };
        fifo[wr] = c;
        u.wr.store(next, Ordering::Relaxed);
    }
}

#[no_mangle]
pub extern "C" fn pi_uart_base() -> u32 {
    PI_UART_BASE
}

#[no_mangle]
pub extern "C" fn pi_cons_push(c: u32) {
    if c <= 0xFF {
        push(c as u8);
    }
}

#[no_mangle]
pub extern "C" fn pi_cons_poll() -> u32 {
    let u = &UART;
    let rd = u.rd.load(Ordering::Relaxed);
    if rd == u.wr.load(Ordering::Relaxed) {
        return EMPTY;
    }
    let fifo = unsafe { &mut *u.fifo.get() };
    let c = fifo[rd];
    u.rd.store((rd + 1) % FIFO_LEN, Ordering::Relaxed);
    c as u32
}

#[no_mangle]
pub extern "C" fn pi_kernel_len() -> u32 {
    KERNEL.len() as u32
}

const KERNEL: &[u8] = &[
    // TX-fifo console protocol: the kernel stores 32-bit words into the UART
    // device window, one char per slot, slots stride 4 bytes (chars are kept
    // word-aligned because this unicorn.js build drops unaligned stores and
    // mis-decodes `and` register-immediate on r0).
    //
    // movw r0, #0x1000            -> 0xE3010000
    0x00, 0x00, 0x01, 0xE3,
    // movt r0, #0x3F20            -> 0xE3430F20   (r0 = 0x3F201000, UART base)
    0x20, 0x0F, 0x43, 0xE3,
    // mov  r1, #'H'               -> 0xE3A01048
    0x48, 0x10, 0xA0, 0xE3,
    // str  r1, [r0]               -> 0xE5801000
    0x00, 0x10, 0x80, 0xE5,
    // add  r0, r0, #4             -> 0xE2800004
    0x04, 0x00, 0x80, 0xE2,
    // mov  r1, #'i'               -> 0xE3A01069
    0x69, 0x10, 0xA0, 0xE3,
    // str  r1, [r0]               -> 0xE5801000
    0x00, 0x10, 0x80, 0xE5,
    // add  r0, r0, #4             -> 0xE2800004
    0x04, 0x00, 0x80, 0xE2,
    // mov  r1, #'\n'              -> 0xE3A0100A
    0x0A, 0x10, 0xA0, 0xE3,
    // str  r1, [r0]               -> 0xE5801000
    0x00, 0x10, 0x80, 0xE5,
    // add  r0, r0, #4             -> 0xE2800004
    0x04, 0x00, 0x80, 0xE2,
    // b .                         -> 0xEAFFFFFE
    0xFE, 0xFF, 0xFF, 0xEA,
];

#[no_mangle]
pub extern "C" fn pi_kernel() -> u32 {
    KERNEL.as_ptr() as u32
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}