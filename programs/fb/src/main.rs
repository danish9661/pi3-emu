#![no_std]
#![no_main]

//! Framebuffer demo: ask the VideoCore (host) for a 160x120x32 screen via
//! the mailbox, draw a red screen with a blue border (deterministic test
//! pattern), then run a bouncing-ball animation paced by the system timer.
//!
//! The host allocates the framebuffer at 0x200000 (inside guest RAM) and
//! blits it to a canvas every animation frame; pixels are 32-bit RGB
//! (byte0=R, byte1=G, byte2=B, byte3=0), little-endian.

use pi_runtime::{puts, putu, putx};

const FB_W: u32 = 160;
const FB_H: u32 = 120;

// VideoCore mailbox (same layout as the shell's `mbox` command).
const MBOX_BASE: u32 = 0x3F00_B880;
const MBOX_READ: u32 = MBOX_BASE + 0x00;
const MBOX_STATUS: u32 = MBOX_BASE + 0x04;
const MBOX_MAIL1_WRITE: u32 = MBOX_BASE + 0x14;
const MBOX_MAIL1_STATUS: u32 = MBOX_BASE + 0x18;
const MBOX_CHANNEL: u32 = 8;

#[repr(C, align(16))]
struct MboxBuf {
    data: [u8; 256],
}
#[unsafe(link_section = ".mbox")]
static mut MB: MboxBuf = MboxBuf { data: [0; 256] };

const TMR_CLO: u32 = 0x3F00_3004;

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

fn now_us() -> u32 {
    mmio_read(TMR_CLO)
}

/// Append a request tag (id + value bytes, 4-byte aligned), return new offset.
fn mb_tag(buf: &mut [u8], mut off: usize, id: u32, value: &[u8]) -> usize {
    buf[off..off + 4].copy_from_slice(&id.to_le_bytes());
    buf[off + 4..off + 8].copy_from_slice(&(value.len() as u32).to_le_bytes());
    buf[off + 8..off + 12].copy_from_slice(&0u32.to_le_bytes()); // request
    buf[off + 12..off + 12 + value.len()].copy_from_slice(value);
    off += 12 + value.len();
    off += (4 - off % 4) % 4;
    off
}

/// Send the request buffer over channel 8; true on success.
fn mb_call() -> bool {
    unsafe {
        while mmio_read(MBOX_MAIL1_STATUS) & 0x8000_0000 != 0 {}
        mmio_write(
            MBOX_MAIL1_WRITE,
            (core::ptr::addr_of!(MB) as *const _ as u32) | MBOX_CHANNEL,
        );
        while mmio_read(MBOX_STATUS) & 0x8000_0000 != 0 {}
        let r = mmio_read(MBOX_READ);
        if r & 0xF != MBOX_CHANNEL {
            return false;
        }
        let code = u32::from_le_bytes(MB.data[4..8].try_into().unwrap());
        code & 0x8000_0000 != 0
    }
}

/// Find a tag's response value bytes in the buffer.
fn mb_find(id: u32) -> Option<(usize, usize)> {
    unsafe {
        let size = u32::from_le_bytes(MB.data[0..4].try_into().unwrap()) as usize;
        let mut off = 8;
        while off + 8 <= size {
            let tid = u32::from_le_bytes(MB.data[off..off + 4].try_into().unwrap());
            let tsize = u32::from_le_bytes(MB.data[off + 4..off + 8].try_into().unwrap()) as usize;
            if tid == 0 {
                break;
            }
            if tid == id {
                return Some((off + 12, tsize));
            }
            off += 12 + tsize + ((4 - tsize % 4) % 4);
        }
    }
    None
}

// --- framebuffer drawing ------------------------------------------------

fn put_pixel(fb: usize, pitch: usize, x: u32, y: u32, r: u8, g: u8, b: u8) {
    unsafe {
        let p = (fb + (y as usize) * pitch + (x as usize) * 4) as *mut u32;
        *p = (r as u32) | ((g as u32) << 8) | ((b as u32) << 16);
    }
}

fn fill(fb: usize, pitch: usize, r: u8, g: u8, b: u8) {
    for y in 0..FB_H {
        for x in 0..FB_W {
            put_pixel(fb, pitch, x, y, r, g, b);
        }
    }
}

fn rect(fb: usize, pitch: usize, x0: u32, y0: u32, x1: u32, y1: u32, r: u8, g: u8, b: u8) {
    for x in x0..=x1 {
        put_pixel(fb, pitch, x, y0, r, g, b);
        put_pixel(fb, pitch, x, y1, r, g, b);
    }
    for y in y0 + 1..y1 {
        put_pixel(fb, pitch, x0, y, r, g, b);
        put_pixel(fb, pitch, x1, y, r, g, b);
    }
}

fn ball(fb: usize, pitch: usize, cx: i32, cy: i32, rad: i32, r: u8, g: u8, b: u8) {
    for dy in -rad..=rad {
        for dx in -rad..=rad {
            if dx * dx + dy * dy <= rad * rad {
                let x = cx + dx;
                let y = cy + dy;
                if x >= 0 && y >= 0 && (x as u32) < FB_W && (y as u32) < FB_H {
                    put_pixel(fb, pitch, x as u32, y as u32, r, g, b);
                }
            }
        }
    }
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
    // property request: 160x120x32 RGB, then allocate + query the pitch
    unsafe {
        let b: &mut [u8] = unsafe { &mut *(&raw mut MB.data) };
        let mut wh = [0u8; 8];
        wh[0..4].copy_from_slice(&FB_W.to_le_bytes());
        wh[4..8].copy_from_slice(&FB_H.to_le_bytes());
        let mut off = 8;
        off = mb_tag(b, off, 0x0004_8003, &wh); // set physical W/H
        off = mb_tag(b, off, 0x0004_8004, &wh); // set virtual W/H
        off = mb_tag(b, off, 0x0004_8005, &32u32.to_le_bytes()); // depth
        off = mb_tag(b, off, 0x0004_8006, &0u32.to_le_bytes()); // pixel order: RGB
        off = mb_tag(b, off, 0x0004_0001, &16u32.to_le_bytes()); // allocate, 16-byte aligned
        off = mb_tag(b, off, 0x0004_0008, &[0u8; 8]); // get pitch
        b[off..off + 4].fill(0);
        off += 4;
        b[0..4].copy_from_slice(&(off as u32).to_le_bytes());
        b[4..8].copy_from_slice(&0u32.to_le_bytes());
        if !mb_call() {
            puts("\rfb: mailbox failed\r\n");
            loop {}
        }
        let (alloc_off, _) = mb_find(0x0004_0001).unwrap();
        let fb = u32::from_le_bytes(b[alloc_off..alloc_off + 4].try_into().unwrap()) as usize;
        let pitch = u32::from_le_bytes(b[alloc_off + 4..alloc_off + 8].try_into().unwrap()) as usize;

        puts("fb: ");
        putu(FB_W as u64);
        puts("x");
        putu(FB_H as u64);
        puts(" pitch ");
        putu(pitch as u64);
        puts(" @ 0x");
        putx(fb as u64);
        puts("\r\n");

        // deterministic test pattern: red screen, blue border, yellow ball
        fill(fb, pitch, 0xff, 0x00, 0x00);
        rect(fb, pitch, 2, 2, FB_W - 3, FB_H - 3, 0x00, 0x00, 0xff);
        ball(fb, pitch, (FB_W / 2) as i32, (FB_H / 2) as i32, 8, 0xff, 0xff, 0x00);
        puts("pattern drawn\r\n");

        // animation: bouncing yellow ball, paced by the system timer
        let mut cx = (FB_W / 2) as i32;
        let mut cy = (FB_H / 2) as i32;
        let mut dx = 2i32;
        let mut dy = 1i32;
        let mut t = now_us();
        loop {
            if now_us().wrapping_sub(t) < 33_000 {
                continue;
            }
            t = now_us();
            ball(fb, pitch, cx, cy, 8, 0x00, 0x00, 0x00); // erase
            cx += dx;
            cy += dy;
            if cx < 8 || cx >= FB_W as i32 - 8 {
                dx = -dx;
            }
            if cy < 8 || cy >= FB_H as i32 - 8 {
                dy = -dy;
            }
            ball(fb, pitch, cx, cy, 8, 0xff, 0xff, 0x00);
        }
    }
}