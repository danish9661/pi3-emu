//! Driver support (dep-free port of the tutorial's driver.rs: 2 slots).

use crate::synchronization::{interface::Mutex, NullLock};

const NUM_DRIVERS: usize = 2;

/// Driver interfaces.
pub mod interface {
    /// Device driver functions.
    pub trait DeviceDriver {
        /// Compatibility string identifying the driver.
        fn compatible(&self) -> &'static str;

        /// Bring up the device.
        ///
        /// # Safety
        ///
        /// - During init, drivers might do stuff with system-wide impact.
        unsafe fn init(&self) -> Result<(), &'static str> {
            Ok(())
        }
    }
}

/// A descriptor for a registered device driver.
#[derive(Copy, Clone)]
pub struct Descriptor {
    driver: &'static (dyn interface::DeviceDriver + Sync),
}

impl Descriptor {
    /// Create an instance.
    pub fn new(driver: &'static (dyn interface::DeviceDriver + Sync)) -> Self {
        Self { driver }
    }
}

/// Provides device driver management functions.
pub struct Manager {
    inner: NullLock<[Option<Descriptor>; NUM_DRIVERS]>,
}

static MANAGER: Manager = Manager {
    inner: NullLock::new([None; NUM_DRIVERS]),
};

/// Return a reference to the global driver manager.
pub fn manager() -> &'static Manager {
    &MANAGER
}

impl Manager {
    /// Register a device driver with the kernel.
    pub fn register(&self, d: Descriptor) {
        self.inner.lock(|slots| {
            let i = slots.iter().position(|x| x.is_none()).expect("driver full");
            slots[i] = Some(d);
        })
    }

    /// Fully initialize all drivers.
    ///
    /// # Safety
    ///
    /// - During init, drivers might do stuff with system-wide impact.
    pub unsafe fn init_all(&self) {
        self.inner.lock(|slots| {
            for d in slots.iter().flatten() {
                if let Err(e) = d.driver.init() {
                    panic!("driver {}: {}", d.driver.compatible(), e);
                }
            }
        })
    }

    /// Enumerate all registered device drivers.
    pub fn enumerate(&self) {
        self.inner.lock(|slots| {
            for (i, d) in slots.iter().flatten().enumerate() {
                crate::println!("      {}. {}", i + 1, d.driver.compatible());
            }
        })
    }
}
