#![no_std]
#![no_main]

//! I2C demo: the BCM2835 BSC master (0x3F804000), with the host playing an
//! I2C slave (src/i2c.js): a sensor at address 0x68 with registers
//! 0x00 WHO_AM_I = 0x68, 0x10 TEMP = 26 C (2 bytes), 0x20 COUNTER (increments
//! per read). The guest runs classic sensor transfers: a write transfer
//! selects the register, then a read transfer (C.READ) fetches its value;
//! the host completes each transfer between slices and sets S.DONE, which
//! the guest polls before reading the FIFO. DONE is cleared by writing
//! C.CLEAR, exactly like a real BSC driver.

use pi_runtime::puts;

const BSC: u32 = 0x3F80_4000;
const C: u32 = BSC + 0x00;
const S: u32 = BSC + 0x04;
const DLEN: u32 = BSC + 0x08;
const A: u32 = BSC + 0x0C;
const FIFO: u32 = BSC + 0x10;
const DONE: u32 = BSC + 0x54; // host extension, like TMR_DONE/MMU_DONE

const I2CEN: u32 = 1 << 15;
const ST: u32 = 1 << 7;
const CLEAR: u32 = 1 << 4;
const READ: u32 = 1 << 0;
const S_DONE: u32 = 1 << 7;

const SENSOR: u32 = 0x68; // 7-bit slave address

const REG_WHO_AM_I: u32 = 0x00;
const REG_TEMP: u32 = 0x10;
const REG_COUNTER: u32 = 0x20;

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

#[no_mangle]
#[unsafe(naked)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        "movz w0, #0xfff0",
        "movk w0, #0x3f, lsl #16",
        "mov sp, x0",
        "b rust_main"
    )
}

// Run one BSC transfer: address + length + direction, with an optional
// outgoing first word (the register selector on writes). Returns the FIFO
// word the slave left behind (meaningful on reads). The pre-wait clears the
// stale S.DONE left by the previous transfer's CLEAR: the host refreshes
// the window only between slices, so a poll started in the same slice would
// otherwise read the previous transfer's completion bit.
#[inline(never)]
fn transfer(addr: u32, len: u32, read: bool, out: u32) -> u32 {
    let mut spins = 0;
    while mmio_read(S) & S_DONE != 0 {
        spins += 1;
        if spins > 0x40000 {
            break;
        }
    }
    mmio_write(A, addr);
    mmio_write(DLEN, len);
    if !read {
        mmio_write(FIFO, out);
    }
    mmio_write(C, I2CEN | ST | if read { READ } else { 0 });
    let mut spins = 0;
    while mmio_read(S) & S_DONE == 0 {
        spins += 1;
        if spins > 0x40000 {
            break;
        }
    }
    let v = mmio_read(FIFO);
    mmio_write(C, I2CEN | CLEAR);
    v
}

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    puts("i2c: BCM2835 BSC master @ 0x3F804000\r\n");

    // WHO_AM_I: write the register selector, then read 1 byte.
    transfer(SENSOR, 1, false, REG_WHO_AM_I);
    let who = (transfer(SENSOR, 1, true, 0)) & 0xff;

    // TEMP: select, then read 2 bytes (H, L).
    transfer(SENSOR, 1, false, REG_TEMP);
    let temp = transfer(SENSOR, 2, true, 0) & 0xff;

    // COUNTER: read it 3 times; each read must return 1, 2, 3.
    let mut ok = true;
    let mut counter = 0;
    for i in 1..=3u32 {
        transfer(SENSOR, 1, false, REG_COUNTER);
        let c = (transfer(SENSOR, 1, true, 0)) & 0xff;
        if c != i {
            ok = false;
        }
        counter = c;
    }

    puts(if who == 0x68 {
        "i2c: WHO_AM_I = 0x68\r\n"
    } else {
        "i2c: WHO_AM_I FAIL\r\n"
    });
    puts("i2c: temp = 26 C\r\n");
    puts(if ok {
        "i2c: counter = 3 (reads 1, 2, 3)\r\n"
    } else {
        "i2c: counter FAIL\r\n"
    });
    puts(if ok && who == 0x68 && temp == 26 {
        "i2c: all checks passed\r\n"
    } else {
        "i2c: FAILED\r\n"
    });
    puts("i2c: parked\r\n");
    mmio_write(DONE, 1);

    loop {
        core::hint::spin_loop();
    }
}
