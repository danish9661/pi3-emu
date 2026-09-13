//! Synchronization primitives (dep-free port of the tutorial's
//! synchronization.rs: NullLock only — single core, interrupts masked).

use core::cell::UnsafeCell;

/// Synchronization interfaces.
pub mod interface {
    /// Exclusive access to the wrapped data for the closure's duration.
    pub trait Mutex {
        /// The wrapped data type.
        type Data;

        /// Lock and grant temporary mutable access.
        fn lock<'a, R>(&'a self, f: impl FnOnce(&'a mut Self::Data) -> R) -> R;
    }
}

/// A pseudo-lock for teaching purposes (single-threaded use only).
pub struct NullLock<T>
where
    T: ?Sized,
{
    data: UnsafeCell<T>,
}

unsafe impl<T> Send for NullLock<T> where T: ?Sized + Send {}
unsafe impl<T> Sync for NullLock<T> where T: ?Sized + Send {}

impl<T> NullLock<T> {
    /// Create an instance.
    pub const fn new(data: T) -> Self {
        Self {
            data: UnsafeCell::new(data),
        }
    }
}

impl<T> interface::Mutex for NullLock<T> {
    type Data = T;

    fn lock<'a, R>(&'a self, f: impl FnOnce(&'a mut Self::Data) -> R) -> R {
        f(unsafe { &mut *self.data.get() })
    }
}
