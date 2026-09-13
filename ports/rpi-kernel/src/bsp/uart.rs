//! PL011 UART driver (dep-free port of the tutorial's
//! bcm2xxx_pl011_uart.rs: raw volatile MMIO, no tock-registers).
//!
//! Init sequence matches pi-cpu Bus expectations (cpu/src/lib.rs UART0
//! window): CR=0, ICR=0x7FF, IBRD=1, FBRD=40 (115200 @ 3 MHz),
//! LCRH=0x70 (8N1+FEN), CR=0x301 (UARTEN|TXE|RXE). FR bit5 TXFF never
//! sets and bit3 BUSY never sets in the model, so TXFF/BUSY spins are
//! instant no-ops; CR must keep bit9+bit0 or test keys get dropped.

use crate::{
    console, driver,
    synchronization::{interface::Mutex, NullLock},
};
use core::fmt;

const DR: usize = 0x00;
const FR: usize = 0x18;
const IBRD: usize = 0x24;
const FBRD: usize = 0x28;
const LCRH: usize = 0x2C;
const CR: usize = 0x30;
const ICR: usize = 0x44;

const TXFF: u32 = 1 << 5;
const RXFE: u32 = 1 << 4;
const BUSY: u32 = 1 << 3;

struct Inner {
    base: usize,
}

/// Representation of the UART.
pub struct PL011 {
    inner: NullLock<Inner>,
}

impl Inner {
    const fn new(base: usize) -> Self {
        Self { base }
    }

    fn rd(&self, off: usize) -> u32 {
        unsafe { core::ptr::read_volatile((self.base + off) as *const u32) }
    }

    fn wr(&self, off: usize, v: u32) {
        unsafe { core::ptr::write_volatile((self.base + off) as *mut u32, v) }
    }

    fn init(&mut self) {
        while self.rd(FR) & BUSY != 0 {
            core::hint::spin_loop();
        }
        self.wr(CR, 0);
        self.wr(ICR, 0x7FF);
        self.wr(IBRD, 1);
        self.wr(FBRD, 40);
        self.wr(LCRH, 0x70);
        self.wr(CR, 0x301);
    }

    fn putc(&mut self, c: u8) {
        while self.rd(FR) & TXFF != 0 {
            core::hint::spin_loop();
        }
        self.wr(DR, c as u32);
    }

    fn getc(&mut self) -> char {
        while self.rd(FR) & RXFE != 0 {
            core::hint::spin_loop();
        }
        let c = (self.rd(DR) & 0xFF) as u8 as char;
        if c == '\r' {
            '\n'
        } else {
            c
        }
    }

    fn clear_rx(&mut self) {
        while self.rd(FR) & RXFE == 0 {
            self.rd(DR);
        }
    }
}

impl fmt::Write for Inner {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.putc(b);
        }
        Ok(())
    }
}

impl PL011 {
    pub const COMPATIBLE: &'static str = "BCM PL011 UART";

    /// Create an instance.
    ///
    /// # Safety
    ///
    /// - The user must ensure to provide a correct MMIO start address.
    pub const unsafe fn new(base: usize) -> Self {
        Self {
            inner: NullLock::new(Inner::new(base)),
        }
    }
}

impl driver::interface::DeviceDriver for PL011 {
    fn compatible(&self) -> &'static str {
        Self::COMPATIBLE
    }

    unsafe fn init(&self) -> Result<(), &'static str> {
        self.inner.lock(|i| i.init());
        Ok(())
    }
}

impl console::interface::Write for PL011 {
    fn write_char(&self, c: char) {
        self.inner.lock(|i| i.putc(c as u8));
    }

    fn write_fmt(&self, args: fmt::Arguments) -> fmt::Result {
        self.inner.lock(|i| fmt::Write::write_fmt(i, args))
    }

    fn flush(&self) {
        while self.inner.lock(|i| i.rd(FR)) & BUSY != 0 {
            core::hint::spin_loop();
        }
    }
}

impl console::interface::Read for PL011 {
    fn read_char(&self) -> char {
        self.inner.lock(|i| i.getc())
    }

    fn clear_rx(&self) {
        self.inner.lock(|i| i.clear_rx())
    }
}

impl console::interface::All for PL011 {}
