#![no_std]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub const PI_UART_BASE: u32 = 0x3F20_1000;
pub const PI_UART_WINDOW: u32 = 0x1000;
pub const KERNEL_ADDR: u32 = 0x80000;
pub const LINEBUF_ADDR: u32 = 0x8000;

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
// M4: the guest owns every decision.  A single self-contained AArch64 kernel
// (loaded at KERNEL_ADDR) polls the RX slot in a loop, echoes each key,
// keeps a line buffer in RAM, and dispatches commands with real conditional
// branches.  The host only delivers keystrokes to the RX slot, runs a bounded
// slice of the kernel, and drains TX slots to the console FIFO.
//
// Device window (0x3F201000, 4 KiB):
//   +0x00..  TX slots, one char per word (16 slots, host drains)
//   +0x80    RX slot  (host writes a byte, guest kernel consumes)
//
// Kernel registers (stable across host slices):
//   x26 = UART base    x24 = line buffer base    x27 = line write ptr
//   x28 = line length (words)
//
// Instruction encodings are a deliberate subset, every one verified against
// public/unicorn.js (official v2.1.4):
//   movz/movk (w + x), add x, subs x, cmp x, csel x, ldr w / str w / str x
//   unsigned-offset (imm12 is scaled: /4 for w, /8 for x), cbz w, b.eq/b.ne, b
// ---------------------------------------------------------------------------

const MAX_WORDS: usize = 512;
const MAX_PENDING: usize = 64;
const MAX_LABELS: usize = 16;

// Label indices, in the order a.label() is called in build_kernel.
const BOOT: u32 = 0;
const POLL: u32 = 1;
const H_CR: u32 = 2;
const CHK_RPI: u32 = 3;
const CHK_HELP: u32 = 4;
const CHK_VER: u32 = 5;
const P_UNK: u32 = 6;
const P_HI: u32 = 7;
const P_RPI: u32 = 8;
const P_HELP: u32 = 9;
const P_VER: u32 = 10;
const DONE: u32 = 11;
const H_BS: u32 = 12;

const X0: u32 = 0;
const X1: u32 = 1;
const X2: u32 = 2;
const X3: u32 = 3;
const X24: u32 = 24;
const X26: u32 = 26;
const X27: u32 = 27;
const X28: u32 = 28;
const XZR: u32 = 31;

struct Asm {
    w: [u32; MAX_WORDS],
    n: usize,
    pending: [(u32, u32); MAX_PENDING],
    pend_n: usize,
    labels: [u32; MAX_LABELS],
    label_n: usize,
}

impl Asm {
    fn new() -> Self {
        Asm {
            w: [0; MAX_WORDS],
            n: 0,
            pending: [(0, 0); MAX_PENDING],
            pend_n: 0,
            labels: [0; MAX_LABELS],
            label_n: 0,
        }
    }

    fn emit(&mut self, v: u32) {
        self.w[self.n] = v;
        self.n += 1;
    }

    fn label(&mut self) -> u32 {
        let l = self.label_n as u32;
        self.labels[self.label_n] = self.n as u32;
        self.label_n += 1;
        l
    }

    fn branch(&mut self, instr: u32, target: u32) {
        self.pending[self.pend_n] = (self.n as u32, target);
        self.pend_n += 1;
        self.emit(instr);
    }

    fn movz(&mut self, rd: u32, imm: u32) {
        self.emit(0xD280_0000 | ((imm & 0xFFFF) << 5) | rd);
    }

    fn movk(&mut self, rd: u32, imm: u32, hw: u32) {
        self.emit(0xF280_0000 | (hw << 21) | ((imm & 0xFFFF) << 5) | rd);
    }

    fn add(&mut self, rd: u32, rn: u32, imm: u32) {
        self.emit(0x9100_0000 | ((imm & 0xFFF) << 10) | (rn << 5) | rd);
    }

    fn subs(&mut self, rd: u32, rn: u32, imm: u32) {
        self.emit(0xF100_0000 | ((imm & 0xFFF) << 10) | (rn << 5) | rd);
    }

    fn cmp(&mut self, rn: u32, imm: u32) {
        self.subs(XZR, rn, imm);
    }

    fn orr(&mut self, rd: u32, rn: u32, rm: u32) {
        self.emit(0xAA00_0000 | (rm << 16) | (rn << 5) | rd);
    }

    fn csel(&mut self, rd: u32, rn: u32, rm: u32, cond: u32) {
        self.emit(0x9A80_0000 | (rm << 16) | (cond << 12) | (rn << 5) | rd);
    }

    fn ldrw(&mut self, rt: u32, rn: u32, off: u32) {
        self.emit(0xB940_0000 | (((off >> 2) & 0xFFF) << 10) | (rn << 5) | rt);
    }

    fn strw(&mut self, rt: u32, rn: u32, off: u32) {
        self.emit(0xB900_0000 | (((off >> 2) & 0xFFF) << 10) | (rn << 5) | rt);
    }

    fn strx(&mut self, rt: u32, rn: u32, off: u32) {
        self.emit(0xF900_0000 | (((off >> 3) & 0xFFF) << 10) | (rn << 5) | rt);
    }

    fn beq(&mut self, t: u32) {
        self.branch(0x5400_0000, t);
    }

    fn bne(&mut self, t: u32) {
        self.branch(0x5400_0001, t);
    }

    fn cbzw(&mut self, rt: u32, t: u32) {
        self.branch(0x3400_0000 | rt, t);
    }

    fn b(&mut self, t: u32) {
        self.branch(0x1400_0000, t);
    }

    /// movz w1,#c ; str w1,[x26,#4i]  for each char (TX slot per char).
    fn emit_text(&mut self, text: &[u8]) {
        for (i, c) in text.iter().enumerate() {
            self.movz(X1, *c as u32);
            self.strw(X1, X26, (i * 4) as u32);
        }
    }

    /// Like emit_text but chars start at TX slot `base` instead of slot 0.
    fn emit_text_at(&mut self, base: usize, text: &[u8]) {
        for (i, c) in text.iter().enumerate() {
            self.movz(X1, *c as u32);
            self.strw(X1, X26, ((base + i) * 4) as u32);
        }
    }

    fn resolve(&mut self) {
        for i in 0..self.pend_n {
            let (idx, target) = self.pending[i];
            let off = self.labels[target as usize] as i32 - idx as i32;
            let word = self.w[idx as usize];
            let (mask, shift) = match word & 0x7F00_0000 {
                0x1400_0000 => (0x03FF_FFFFu32, 0u32), // b (imm26)
                _ => (0x0007_FFFFu32, 5u32),           // b.cond / cbz (imm19)
            };
            self.w[idx as usize] = (word & !(mask << shift)) | (((off as u32) & mask) << shift);
        }
    }
}

fn build_kernel(a: &mut Asm) {
    a.label(); // BOOT
    a.movz(X26, 0x1000);
    a.movk(X26, 0x3F20, 1); // x26 = 0x3F20_1000
    a.movz(X24, LINEBUF_ADDR);
    a.movz(X3, 0x20); // X3 = 0x20, used to lowercase chars
    a.add(X27, X24, 0);
    a.movz(X28, 0);
    a.emit_text_at(0, b"Hi\n");
    a.b(DONE); // boot prompt, then poll

    a.label(); // POLL
    a.ldrw(X0, X26, 0x80); // RX slot
    a.cbzw(X0, POLL);
    a.strx(XZR, X26, 0x80); // consume RX
    a.cmp(X0, 0x7F); // Backspace: fix buffer only (host fixes display)
    a.beq(H_BS);
    a.cmp(X0, 13);
    a.beq(H_CR);
    a.strw(X0, X26, 0); // echo printable to TX slot 0
    a.strw(X0, X27, 0); // append char word to line buffer
    a.add(X27, X27, 4);
    a.add(X28, X28, 1);
    a.b(POLL);

    a.label(); // H_CR
    a.strw(X0, X26, 0); // echo CR to TX slot 0 (response starts at slot 1)
    a.add(X27, X24, 0); // reset write pointer
    a.cmp(X28, 0); // empty line: just reprompt, no "?"
    a.beq(DONE);
    // command: HI (len 2) — case-insensitive: chars are OR'd with 0x20 (lowercase)
    a.cmp(X28, 2);
    a.bne(CHK_RPI);
    a.ldrw(X2, X24, 0);
    a.orr(X2, X2, X3);
    a.cmp(X2, 0x68); // 'h'
    a.bne(CHK_RPI);
    a.ldrw(X2, X24, 4);
    a.orr(X2, X2, X3);
    a.cmp(X2, 0x69); // 'i'
    a.bne(CHK_RPI);
    a.b(P_HI);

    a.label(); // CHK_RPI
    a.cmp(X28, 3);
    a.bne(CHK_HELP);
    a.ldrw(X2, X24, 0);
    a.orr(X2, X2, X3);
    a.cmp(X2, 0x72); // 'r'
    a.bne(CHK_HELP);
    a.ldrw(X2, X24, 4);
    a.orr(X2, X2, X3);
    a.cmp(X2, 0x70); // 'p'
    a.bne(CHK_HELP);
    a.ldrw(X2, X24, 8);
    a.orr(X2, X2, X3);
    a.cmp(X2, 0x69); // 'i'
    a.bne(CHK_HELP);
    a.b(P_RPI);

    a.label(); // CHK_HELP
    a.cmp(X28, 4);
    a.bne(CHK_VER);
    a.ldrw(X2, X24, 0);
    a.orr(X2, X2, X3);
    a.cmp(X2, 0x68); // 'h'
    a.bne(CHK_VER);
    a.ldrw(X2, X24, 4);
    a.orr(X2, X2, X3);
    a.cmp(X2, 0x65); // 'e'
    a.bne(CHK_VER);
    a.ldrw(X2, X24, 8);
    a.orr(X2, X2, X3);
    a.cmp(X2, 0x6C); // 'l'
    a.bne(CHK_VER);
    a.ldrw(X2, X24, 12);
    a.orr(X2, X2, X3);
    a.cmp(X2, 0x70); // 'p'
    a.bne(CHK_VER);
    a.b(P_HELP);

    a.label(); // CHK_VER
    a.cmp(X28, 3);
    a.bne(P_UNK);
    a.ldrw(X2, X24, 0);
    a.orr(X2, X2, X3);
    a.cmp(X2, 0x76); // 'v'
    a.bne(P_UNK);
    a.ldrw(X2, X24, 4);
    a.orr(X2, X2, X3);
    a.cmp(X2, 0x65); // 'e'
    a.bne(P_UNK);
    a.ldrw(X2, X24, 8);
    a.orr(X2, X2, X3);
    a.cmp(X2, 0x72); // 'r'
    a.bne(P_UNK);
    a.b(P_VER);

    a.label(); // P_UNK
    a.emit_text_at(1, b"?\r\n");
    a.b(DONE);

    a.label(); // P_HI
    a.emit_text_at(1, b"HELLO\r\n");
    a.b(DONE);

    a.label(); // P_RPI
    a.emit_text_at(1, b"Raspberry Pi 3\r\n");
    a.b(DONE);

    a.label(); // P_HELP
    a.emit_text_at(1, b"hi, rpi, help, ver\r\n");
    a.b(DONE);

    a.label(); // P_VER
    a.emit_text_at(1, b"pi3-emu v1.0\r\n");
    a.b(DONE);

    a.label(); // DONE
    a.movz(X28, 0);
    a.emit_text_at(17, b"> ");
    a.b(POLL);

    a.label(); // H_BS
    a.cmp(X28, 0);
    a.beq(POLL);
    a.subs(X28, X28, 1);
    a.subs(X27, X27, 4);
    a.b(POLL);

    a.resolve();
}

static mut KERNEL: [u8; MAX_WORDS * 4] = [0; MAX_WORDS * 4];
static KERNEL_LEN: AtomicUsize = AtomicUsize::new(0);
static KERNEL_READY: AtomicBool = AtomicBool::new(false);

fn build() {
    let mut a = Asm::new();
    build_kernel(&mut a);
    let kbuf = unsafe { &mut *core::ptr::addr_of_mut!(KERNEL) };
    for i in 0..a.n {
        let v = a.w[i];
        kbuf[i * 4] = v as u8;
        kbuf[i * 4 + 1] = (v >> 8) as u8;
        kbuf[i * 4 + 2] = (v >> 16) as u8;
        kbuf[i * 4 + 3] = (v >> 24) as u8;
    }
    KERNEL_LEN.store(a.n * 4, Ordering::Relaxed);
}

#[no_mangle]
pub extern "C" fn pi_kernel() -> u32 {
    if !KERNEL_READY.load(Ordering::Relaxed) {
        build();
        KERNEL_READY.store(true, Ordering::Relaxed);
    }
    unsafe { KERNEL.as_ptr() as u32 }
}

#[no_mangle]
pub extern "C" fn pi_kernel_len() -> u32 {
    if !KERNEL_READY.load(Ordering::Relaxed) {
        build();
        KERNEL_READY.store(true, Ordering::Relaxed);
    }
    KERNEL_LEN.load(Ordering::Relaxed) as u32
}

#[no_mangle]
pub extern "C" fn pi_rx_offset() -> u32 {
    0x80
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
