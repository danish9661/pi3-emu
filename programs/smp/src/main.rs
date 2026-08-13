#![no_std]
#![no_main]

//! SMP demo: 4 AArch64 cores booting from dead RAM, coordinated through a
//! shared MMIO mailbox window (0x3F202000) that the host arbitrates between
//! cores — like a bus master serializing device-cache traffic.
//!
//! Device window (0x3F202000, 4 KiB — host-arbitrated mailbox):
//!   +0x00  START_ENTRY[3]  core 0 writes entry addresses for cores 1..3
//!   +0x10  GO              core 0 releases the secondaries (set to 1)
//!   +0x14  COUNTER         shared counter, device-serialized increment
//!   +0x18  LOCK            spinlock flag (0 = free)
//!   +0x1C  MSG[4]          per-core result mailbox (u32 each)
//!   +0x30  CURRENT         host writes the running core id (observability)
//!   +0x34  PARK_MASK       core i sets bit i when done
//!   +0x38  CPUID           host writes the core id (device-provided identity)
//!
//! Each core runs in its own memory partition (per-core RAM at the same
//! addresses, like partitioned DDR) and all cores' UART TX slots drain into
//! the one console FIFO, so the host schedules their output slices in
//! round-robin order.

use pi_runtime::{puts, putu};

const SMP: u32 = 0x3F20_2000;
const START_ENTRY: u32 = SMP + 0x00;
const GO: u32 = SMP + 0x10;
const COUNTER: u32 = SMP + 0x14;
const LOCK: u32 = SMP + 0x18;
const MSG: u32 = SMP + 0x1C;
const PARK_MASK: u32 = SMP + 0x34;
const CPUID: u32 = SMP + 0x38;

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

fn cpuid() -> u32 {
    mmio_read(CPUID)
}

/// Sum of the id-th quarter of 1..=100: core i sums (i*25+1)..(i+1)*25.
fn task_sum(id: u32) -> u64 {
    let lo = id * 25 + 1;
    let hi = lo + 24;
    let mut s: u64 = 0;
    let mut i = lo;
    while i <= hi {
        s += i as u64;
        i += 1;
    }
    s
}

fn lock_acquire() {
    while mmio_read(LOCK) != 0 {
        core::hint::spin_loop();
    }
    mmio_write(LOCK, 1);
}

fn lock_release() {
    mmio_write(LOCK, 0);
}

fn core_report(id: u32, s: u64) {
    lock_acquire();
    puts("core ");
    putu(id as u64);
    puts(": sum ");
    putu((id * 25 + 1) as u64);
    puts("..");
    putu((id * 25 + 25) as u64);
    puts(" = ");
    putu(s);
    puts("\r\n");
    lock_release();
}

// Each of the 4 entry points sets its own stack top (per-core stack space)
// and branches into its main. The host starts core 0 at `_start`; cores 1..3
// at the addresses core 0 writes into START_ENTRY.
macro_rules! core_start {
    ($name:ident, $sp_hi:literal) => {
        #[no_mangle]
        #[unsafe(naked)]
        pub extern "C" fn $name() -> ! {
            core::arch::naked_asm!(
                "movz w0, #0xfff0",
                concat!("movk w0, #", $sp_hi, ", lsl #16"),
                "mov sp, x0",
                "b rust_main"
            )
        }
    };
}

core_start!(_start, 0x3f); // 0x3ffff0
core_start!(smp_core1, 0x3e); // 0x3efff0
core_start!(smp_core2, 0x3d); // 0x3dfff0
core_start!(smp_core3, 0x3c); // 0x3cfff0

fn secondary_main() -> ! {
    let id = cpuid();
    while mmio_read(GO) == 0 {
        core::hint::spin_loop();
    }
    let s = task_sum(id);
    mmio_write(MSG + 4 * id, s as u32);
    mmio_write(COUNTER, mmio_read(COUNTER) + 1);
    core_report(id, s);
    mmio_write(PARK_MASK, mmio_read(PARK_MASK) | (1 << id));
    loop {}
}

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    if cpuid() == 0 {
        primary_main()
    } else {
        secondary_main()
    }
}

#[no_mangle]
fn primary_main() -> ! {
    let s = task_sum(0);
    mmio_write(START_ENTRY + 4, (smp_core1 as extern "C" fn() -> !) as usize as u32);
    mmio_write(START_ENTRY + 8, (smp_core2 as extern "C" fn() -> !) as usize as u32);
    mmio_write(START_ENTRY + 12, (smp_core3 as extern "C" fn() -> !) as usize as u32);
    mmio_write(GO, 1);

    mmio_write(MSG, s as u32);
    mmio_write(COUNTER, mmio_read(COUNTER) + 1);
    core_report(0, s);

    // Wait for the secondaries to park (bits 1..3); bit 0 is ours.
    while (mmio_read(PARK_MASK) & 0xE) != 0xE {
        core::hint::spin_loop();
    }

    lock_acquire();
    puts("mailbox: ");
    for i in 0..4 {
        putu(mmio_read(MSG + 4 * i) as u64);
        puts(" ");
    }
    puts("\r\nall cores joined: counter = ");
    putu(mmio_read(COUNTER) as u64);
    puts("\r\n");
    lock_release();

    mmio_write(PARK_MASK, mmio_read(PARK_MASK) | 1);
    loop {}
}