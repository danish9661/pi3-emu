//! System console (dep-free port of the tutorial's console.rs: no
//! statistics, no null console — the UART is registered directly).

use crate::synchronization::{interface::Mutex, NullLock};

/// Console interfaces.
pub mod interface {
    use core::fmt;

    /// Console write functions.
    pub trait Write {
        /// Write a single character.
        fn write_char(&self, c: char);

        /// Write a Rust format string.
        fn write_fmt(&self, args: fmt::Arguments) -> fmt::Result;

        /// Block until the last character hit the TX wire.
        fn flush(&self) {}
    }

    /// Console read functions.
    pub trait Read {
        /// Read a single character (blocking).
        fn read_char(&self) -> char;

        /// Clear RX buffers, if any.
        fn clear_rx(&self) {}
    }

    /// Trait alias for a full-fledged console.
    pub trait All: Write + Read {}
}

static CUR: NullLock<Option<&'static (dyn interface::All + Sync)>> = NullLock::new(None);

/// Register a new console.
pub fn register_console(new_console: &'static (dyn interface::All + Sync)) {
    CUR.lock(|cur| *cur = Some(new_console));
}

/// Return a reference to the currently registered console.
pub fn console() -> &'static dyn interface::All {
    CUR.lock(|cur| cur.expect("no console registered"))
}
