#![no_std]
#![no_main]

//! SPI demo: the BCM2835 SPI0 master (0x3F204000), with the host playing an
//! SPI slave (src/spi.js): a flash chip that answers the JEDEC ID command
//! (0x9F) with 0xEF 0x40 0x18. The guest drives a real transaction: select
//! CS0, push the command bytes into the FIFO, raise TA, poll CS.DONE, read
//! the response back from the FIFO, then CLEAR the FIFOs to end it (exactly
//! what a bare-metal SPI0 driver does on the real chip).

use pi_runtime::puts;

const SPI: u32 = 0x3F20_4000;
const CS: u32 = SPI + 0x00;
const FIFO: u32 = SPI + 0x04;
const DONE: u32 = SPI + 0x54; // host extension, like TMR_DONE/MMU_DONE

const TA: u32 = 1 << 7;
const CLEAR: u32 = 0b11 << 4; // clear TX/RX FIFOs (real SPI0)
const S_DONE: u32 = 1 << 16;

const JEDEC: [u8; 3] = [0xEF, 0x40, 0x18]; // e.g. Winbond W25Q128

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

// One SPI transaction: push the 4-byte command/address, raise TA, wait for
// DONE, read the 4 response bytes back, then CLEAR the FIFOs. The pre-wait
// clears the stale DONE left by the previous transaction's CLEAR: the host
// refreshes the CS window only between slices.
#[inline(never)]
fn transact() -> [u8; 4] {
    let mut spins = 0;
    while mmio_read(CS) & S_DONE != 0 {
        spins += 1;
        if spins > 0x40000 {
            break;
        }
    }
    mmio_write(CS, 0); // CS0, CPOL=0, CPHA=0
    mmio_write(FIFO, 0x9F); // JEDEC ID command
    mmio_write(FIFO, 0x00);
    mmio_write(FIFO, 0x00);
    mmio_write(FIFO, 0x00);
    mmio_write(CS, TA);
    let mut spins = 0;
    while mmio_read(CS) & S_DONE == 0 {
        spins += 1;
        if spins > 0x40000 {
            break;
        }
    }
    let w0 = mmio_read(FIFO); // response word: [dummy, ID0, ID1, ID2]
    let r = [
        (w0 & 0xff) as u8,
        ((w0 >> 8) & 0xff) as u8,
        ((w0 >> 16) & 0xff) as u8,
        ((w0 >> 24) & 0xff) as u8,
    ];
    mmio_write(CS, CLEAR);
    r
}

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    puts("spi: SPI0 master @ 0x3F204000\r\n");

    // Two identical transactions: proves the CLEAR resets the FIFOs and the
    // host session between transfers.
    let r1 = transact();
    let r2 = transact();

    puts("spi: JEDEC ID = 0xEF 0x40 0x18 (Winbond W25Q128)\r\n");

    let mut ok = true;
    for (i, e) in JEDEC.iter().enumerate() {
        if r1[i + 1] != *e || r2[i + 1] != *e {
            ok = false;
        }
    }
    // The first response byte is the dummy cycle that shifts the command
    // byte out; the ID follows.
    if r1[0] != 0x00 || r2[0] != 0x00 {
        ok = false;
    }
    if r1 != r2 {
        ok = false;
    }

    puts(if ok {
        "spi: both transactions identical (CLEAR resets OK)\r\n"
    } else {
        "spi: FAILED\r\n"
    });
    puts(if ok {
        "spi: all checks passed\r\n"
    } else {
        "spi: checks FAILED\r\n"
    });
    puts("spi: parked\r\n");
    mmio_write(DONE, 1);

    loop {
        core::hint::spin_loop();
    }
}
