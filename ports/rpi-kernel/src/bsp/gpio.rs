//! GPIO driver (dep-free port of the tutorial's bcm2xxx_gpio.rs:
//! pins 14/15 to ALT0 for the PL011; the PUD dance is absorbed by
//! pi-cpu's Bus but kept for hardware fidelity).

use crate::{
    driver,
    synchronization::{interface::Mutex, NullLock},
};

struct Inner {
    base: usize,
}

/// Representation of the GPIO HW.
pub struct GPIO {
    inner: NullLock<Inner>,
}

impl GPIO {
    pub const COMPATIBLE: &'static str = "BCM GPIO";

    /// Create an instance.
    ///
    /// # Safety
    ///
    /// - The user must ensure to provide a correct MMIO start address.
    pub const unsafe fn new(base: usize) -> Self {
        Self {
            inner: NullLock::new(Inner { base }),
        }
    }

    /// Map PL011 UART as standard output (TX pin 14, RX pin 15).
    pub fn map_pl011_uart(&self) {
        self.inner.lock(|i| unsafe {
            let fsel1 = (i.base + 0x04) as *mut u32;
            core::ptr::write_volatile(
                fsel1,
                core::ptr::read_volatile(fsel1) | (0b100 << 12) | (0b100 << 15),
            );
            // BCM2837 pull-up/down disable sequence (absorbed by pi-cpu).
            let pud = (i.base + 0x94) as *mut u32;
            let clk = (i.base + 0x98) as *mut u32;
            core::ptr::write_volatile(pud, 0);
            for _ in 0..2000 {
                core::hint::spin_loop();
            }
            core::ptr::write_volatile(clk, (1 << 14) | (1 << 15));
            for _ in 0..2000 {
                core::hint::spin_loop();
            }
            core::ptr::write_volatile(clk, 0);
        })
    }
}

impl driver::interface::DeviceDriver for GPIO {
    fn compatible(&self) -> &'static str {
        Self::COMPATIBLE
    }
}
