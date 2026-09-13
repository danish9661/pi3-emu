//! Board driver list (dep-free port of the tutorial's
//! bsp/raspberrypi/driver.rs: GPIO + UART only).

use crate::{
    bsp::{GPIO_BASE, UART0_BASE},
    console,
    driver::{manager, Descriptor},
};

pub static UART: crate::bsp::uart::PL011 = unsafe { crate::bsp::uart::PL011::new(UART0_BASE) };
pub static GPIOP: crate::bsp::gpio::GPIO = unsafe { crate::bsp::gpio::GPIO::new(GPIO_BASE) };

// M54: the linker script pins .data at 0x90000 (64K-aligned), so the
// adrp/ldr shapes rustc emits for these statics stay in range. If the
// statics ever move, the native run dies with UnmappedData at the adrp
// target (0x90020 family) — check the PHDR map first, not the driver.

/// Board init: map UART pins, register + init drivers, register console.
///
/// # Safety
///
/// - Must run once on the boot core before `println!` is usable.
pub unsafe fn init_drivers() {
    GPIOP.map_pl011_uart();
    manager().register(Descriptor::new(&GPIOP));
    manager().register(Descriptor::new(&UART));
    manager().init_all();
    console::register_console(&UART);
}
