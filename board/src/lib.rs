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
// immediate encodings of movk/movz for >=14-bit immediates, all conditional
// branches (b.eq never takes, b.ne always takes, cbnz/tbz/tbnz never take,
// backward branches crash) and every flag-setting op (cmp/subs) are
// unreliable.  The kernels below therefore use only verified opcodes
// (ldr-literal, ldr/str unsigned-offset, movz small imm, unconditional b),
// and the *host* schedules short slices of them: the guest owns all output,
// the host owns all decisions.
//
// Device window (0x3F201000, 4 KiB):
//   +0x00..  TX slots, one char per word (guest stores, host drains)
//   +0x40    RX slot  (host writes a byte, guest echo procedure consumes)
// ---------------------------------------------------------------------------

/// Assemble a straight-line print procedure for `text`:
///   ldr  x0, [pc, #(2+2N)*4]   literal at word 2+2N
///   (mov  w1, #c; str w1, [x0, #4k]) x N
///   b .                         park
///   .dword 0x3F201000           UART base literal
/// Only verified opcodes are used.  N <= 16 (16 TX slots before the RX slot).
const fn put(out: &mut [u8], at: usize, v: u32) {
    out[at] = v as u8;
    out[at + 1] = (v >> 8) as u8;
    out[at + 2] = (v >> 16) as u8;
    out[at + 3] = (v >> 24) as u8;
}

const fn print_proc<const N: usize>(text: &[u8; N]) -> [u8; 144] {
    let mut out = [0u8; 144];
    put(&mut out, 0, 0x5800_0000 | (((2 + 2 * N) as u32) << 5));
    let mut w = 1usize;
    let mut i = 0usize;
    while i < N {
        put(&mut out, w * 4, 0x5280_0000 | ((text[i] as u32) << 5) | 1);
        w += 1;
        put(&mut out, w * 4, 0xB900_0000 | ((i as u32) << 10) | 1);
        w += 1;
        i += 1;
    }
    put(&mut out, w * 4, 0x1400_0000);
    w += 1;
    put(&mut out, w * 4, 0x3F20_1000);
    put(&mut out, w * 4 + 4, 0);
    out
}

const fn proc_len(chars: usize) -> u32 {
    (4 * (3 + 2 * chars)) as u32
}

const KERNEL_INIT: [u8; 144] = print_proc(b"Hi\n> ");
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

const SHELL_PROMPT: [u8; 144] = print_proc(b"> ");
const SHELL_CMD_HI: [u8; 144] = print_proc(b"HELLO\r\n");
const SHELL_CMD_RPI: [u8; 144] = print_proc(b"Raspberry Pi 3\r\n");
const SHELL_CMD_HELP: [u8; 144] = print_proc(b"hi or rpi\r\n");
const SHELL_UNKNOWN: [u8; 144] = print_proc(b"?\r\n");

const SHELL_ADDRS: [u32; 5] = [0x80300, 0x80400, 0x80500, 0x80600, 0x80700];
const SHELL_PROCS: [&[u8]; 5] = [&SHELL_CMD_HI, &SHELL_CMD_RPI, &SHELL_CMD_HELP, &SHELL_UNKNOWN, &SHELL_PROMPT];
const SHELL_CHARS: [usize; 5] = [7, 16, 11, 3, 2];

#[no_mangle]
pub extern "C" fn pi_kernel_init() -> u32 {
    KERNEL_INIT.as_ptr() as u32
}

#[no_mangle]
pub extern "C" fn pi_kernel_init_len() -> u32 {
    proc_len(5)
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
pub extern "C" fn pi_shell_proc(idx: u32) -> u32 {
    SHELL_PROCS[idx as usize].as_ptr() as u32
}

#[no_mangle]
pub extern "C" fn pi_shell_addr(idx: u32) -> u32 {
    SHELL_ADDRS[idx as usize]
}

#[no_mangle]
pub extern "C" fn pi_shell_len(idx: u32) -> u32 {
    proc_len(SHELL_CHARS[idx as usize])
}

#[no_mangle]
pub extern "C" fn pi_rx_offset() -> u32 {
    0x40
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}