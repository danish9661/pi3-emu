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

// ---------------------------------------------------------------------------
// CPU note: this unicorn.js build has a broken ARM32 decoder (every guest
// *load* raises a trap, and several ALU/load forms mis-decode).  The
// AArch64 core works for loads, stores, ALU and unconditional branches, but
// immediate encodings of movk/movz for >=14-bit immediates and all
// conditional branches are unreliable.  The kernels below therefore use only
// verified opcodes (ldr-literal, ldr/str unsigned-offset, movz small imm,
// add imm, unconditional b), and the *host* schedules short slices of the
// echo procedure whenever a key arrives (guest-side polling is impossible).
//
// Device window (0x3F201000, 4 KiB):
//   +0x00..  TX slots, one char per word (guest stores, host drains)
//   +0x40    RX slot  (host writes a byte, guest echo procedure consumes)
// ---------------------------------------------------------------------------

const KERNEL_INIT: &[u8] = &[
    // Boot greeting: prints "Hi\n> " through the TX window, then parks on `b .`.
    // (r0 walks the TX cursor, chars are the low byte of word-aligned slots.)
    // ldr  x0, [pc, #64]            -> 0x58000200   (literal at 0x80040)
    0x00, 0x02, 0x00, 0x58,
    // mov  w1, #'H' (0x48)          -> 0x52800901
    0x01, 0x09, 0x80, 0x52,
    // str  w1, [x0]                 -> 0xB9000001
    0x01, 0x00, 0x00, 0xB9,
    // add  x0, x0, #4               -> 0x91001000
    0x00, 0x10, 0x00, 0x91,
    // mov  w1, #'i' (0x69)          -> 0x52800D21
    0x21, 0x0D, 0x80, 0x52,
    // str  w1, [x0]                 -> 0xB9000001
    0x01, 0x00, 0x00, 0xB9,
    // add  x0, x0, #4               -> 0x91001000
    0x00, 0x10, 0x00, 0x91,
    // mov  w1, #'\n' (0x0A)         -> 0x52800141
    0x41, 0x01, 0x80, 0x52,
    // str  w1, [x0]                 -> 0xB9000001
    0x01, 0x00, 0x00, 0xB9,
    // add  x0, x0, #4               -> 0x91001000
    0x00, 0x10, 0x00, 0x91,
    // mov  w1, #'>' (0x3E)          -> 0x528007C1
    0xC1, 0x07, 0x80, 0x52,
    // str  w1, [x0]                 -> 0xB9000001
    0x01, 0x00, 0x00, 0xB9,
    // add  x0, x0, #4               -> 0x91001000
    0x00, 0x10, 0x00, 0x91,
    // mov  w1, #' ' (0x20)          -> 0x52800401
    0x01, 0x04, 0x80, 0x52,
    // str  w1, [x0]                 -> 0xB9000001
    0x01, 0x00, 0x00, 0xB9,
    // b .                           -> 0x14000000
    0x00, 0x00, 0x00, 0x14,
    // .dword 0x3F201000             (UART base literal)
    0x00, 0x10, 0x20, 0x3F, 0x00, 0x00, 0x00, 0x00,
];

const KERNEL_ECHO: &[u8] = &[
    // Host-scheduled echo procedure (4 instructions, run on each key):
    //   w1 = [x0 + 0x40]   (RX slot)
    //   [x0]     = w1      (echo into TX slot 0)
    //   [x0+0x40] = xzr    (consume RX)
    // ldr  x0, [pc, #12]            -> 0x58000080   (literal at 0x80110)
    0x80, 0x00, 0x00, 0x58,
    // ldr  w1, [x0, #0x40]          -> 0xB9404001
    0x01, 0x40, 0x40, 0xB9,
    // str  w1, [x0]                 -> 0xB9000001
    0x01, 0x00, 0x00, 0xB9,
    // str  xzr, [x0, #0x40]         -> 0xF900201F
    0x1F, 0x20, 0x00, 0xF9,
    // .dword 0x3F201000             (UART base literal)
    0x00, 0x10, 0x20, 0x3F, 0x00, 0x00, 0x00, 0x00,
];

#[no_mangle]
pub extern "C" fn pi_kernel_init() -> u32 {
    KERNEL_INIT.as_ptr() as u32
}

#[no_mangle]
pub extern "C" fn pi_kernel_init_len() -> u32 {
    KERNEL_INIT.len() as u32
}

#[no_mangle]
pub extern "C" fn pi_kernel_echo() -> u32 {
    KERNEL_ECHO.as_ptr() as u32
}

#[no_mangle]
pub extern "C" fn pi_kernel_echo_len() -> u32 {
    KERNEL_ECHO.len() as u32
}

#[no_mangle]
pub extern "C" fn pi_rx_offset() -> u32 {
    0x40
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}