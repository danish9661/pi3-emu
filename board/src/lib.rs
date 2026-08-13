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
pub extern "C" fn pi_rx_offset() -> u32 {
    0x80
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}