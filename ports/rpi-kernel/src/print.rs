//! Printing (dep-free port of the tutorial's print.rs; stable-only:
//! no nightly `format_args_nl!` — newline appended explicitly).

use crate::console;
use crate::console::interface::Write;

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments) {
    console::console().write_fmt(args).unwrap();
}

/// Prints without a newline.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::print::_print(core::format_args!($($arg)*)));
}

/// Prints with a newline.
#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print::_print(core::format_args!("{}\n", core::format_args!($($arg)*))));
}
