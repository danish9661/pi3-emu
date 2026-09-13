//! Board support package: UART0 + GPIO base addresses + driver statics.

pub mod driver;
pub mod gpio;
pub mod uart;

pub const UART0_BASE: usize = 0x3F20_1000;
pub const GPIO_BASE: usize = 0x3F20_0000;
