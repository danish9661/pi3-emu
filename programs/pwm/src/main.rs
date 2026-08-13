#![no_std]
#![no_main]

//! PWM audio demo: the BCM2835 PWM controller (0x3F20C000) in FIFO mode.
//!
//! The guest enables channel 1 (PWEN1) in FIFO mode (USEF1, M/S enabled
//! MSEN1), clears the FIFO (CLRF1 edge), then pushes square-wave samples
//! for "Twinkle Twinkle Little Star" — one fixed-point phase accumulator
//! per note, all integer math — paced by the FULL1/EMPT1 handshake the
//! host refreshes in STA. The host drains the FIFO into a sample ring
//! (src/pwm.js) and the browser plays it through WebAudio; the probe
//! verifies the exact sample stream. Each word's low 16 bits are a signed
//! sample; the guest writes 0x5fff / 0xffffa000 (+0x5fff / -0x5fff).

use pi_runtime::{puts, putu};

const PWM_BASE: u32 = 0x3F20_C000;
const PWM_CTL: u32 = PWM_BASE + 0x00;
const PWM_STA: u32 = PWM_BASE + 0x04;
const PWM_FIFO: u32 = PWM_BASE + 0x20;
const PWM_DONE: u32 = PWM_BASE + 0x54; // host extension, like TMR_DONE/MMU_DONE

const PWEN1: u32 = 1 << 0;
const MODE1: u32 = 1 << 1;
const USEF1: u32 = 1 << 5;
const CLRF1: u32 = 1 << 6;
const MSEN1: u32 = 1 << 7;
const STA_FULL1: u32 = 1 << 0;
const STA_EMPT1: u32 = 1 << 1;

const FS: u32 = 44100;
const H: u32 = 0x5fff; // high half of the square wave
const L: u32 = 0xffff_a000; // low half (-0x5fff, low 16 bits = 0xa000)

// Twinkle Twinkle Little Star: (frequency Hz, samples). 120 ms per note.
const MELODY: [(u32, u32); 14] = [
    (262, 5292),
    (262, 5292),
    (392, 5292),
    (392, 5292),
    (440, 5292),
    (440, 5292),
    (392, 10584),
    (349, 5292),
    (349, 5292),
    (330, 5292),
    (330, 5292),
    (294, 5292),
    (294, 5292),
    (262, 10584),
];

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

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    puts("pwm: BCM2835 PWM controller @ 0x3F20C000\r\n");

    // FIFO mode, M/S enabled; the host latches these level bits and clears
    // the FIFO on the CLRF1 edge.
    mmio_write(PWM_CTL, PWEN1 | USEF1 | MSEN1 | MODE1);
    mmio_write(PWM_CTL, PWEN1 | USEF1 | MSEN1 | MODE1 | CLRF1);
    puts("pwm: FIFO mode enabled (USEF1|MSEN1), FIFO cleared\r\n");

    let mut total: u32 = 0;
    for (freq, dur) in MELODY {
        puts("pwm: note ");
        putu(freq as u64);
        puts(" Hz\r\n");
        let step = (((freq as u64) << 16) / FS as u64) as u32;
        let mut phase: u32 = 0;
        for _ in 0..dur {
            // Pace on the host-refreshed FULL1 bit: when the FIFO is full
            // the guest waits for the host to drain it (real-PWM behavior).
            while mmio_read(PWM_STA) & STA_FULL1 != 0 {
                core::hint::spin_loop();
            }
            mmio_write(PWM_FIFO, if phase & 0x8000 != 0 { L } else { H });
            phase = phase.wrapping_add(step);
        }
        total += dur;
    }

    // Wait until every pushed sample has been drained into the host ring,
    // so PWM_DONE means "the whole tune is in the ring".
    while mmio_read(PWM_STA) & STA_EMPT1 == 0 {
        core::hint::spin_loop();
    }
    puts("pwm: all notes played (");
    putu(total as u64);
    puts(" samples)\r\n");
    puts("pwm: parked\r\n");
    mmio_write(PWM_DONE, 1);

    loop {
        core::hint::spin_loop();
    }
}
