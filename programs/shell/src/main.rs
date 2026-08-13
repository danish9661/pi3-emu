#![no_std]
#![no_main]

use pi_runtime::{getc, putc, puts, putu, putx};

const LEN: usize = 64;

static mut BUF: [u8; LEN] = [0; LEN];
static mut N: usize = 0;

const TMR_CLO: usize = 0x3F00_3004;

// VideoCore mailbox (0x3F00B880): the host answers property-tags requests.
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

fn mb_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}
fn mb_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
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

/// Send the request buffer over channel 8; true on success. Responses land
/// back in the same buffer (tag codes 0x80000000).
fn mb_call() -> bool {
    unsafe {
        while mb_read(MBOX_MAIL1_STATUS) & 0x8000_0000 != 0 {}
        mb_write(MBOX_MAIL1_WRITE, (core::ptr::addr_of!(MB) as *const _ as u32) | MBOX_CHANNEL);
        while mb_read(MBOX_STATUS) & 0x8000_0000 != 0 {}
        let r = mb_read(MBOX_READ);
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

fn cmd_mbox() {
    unsafe {
        let b = &mut MB.data;
        let mut off = 8;
        off = mb_tag(b, off, 0x0001_0002, &0u32.to_le_bytes()); // board rev
        off = mb_tag(b, off, 0x0001_0003, &0u64.to_le_bytes()); // serial
        off = mb_tag(b, off, 0x0001_0005, &[0u8; 8]); // arm memory
        off = mb_tag(b, off, 0x0003_0002, &0u32.to_le_bytes()); // clock rate
        b[off..off + 4].fill(0); // end-of-tags
        off += 4;
        b[0..4].copy_from_slice(&(off as u32).to_le_bytes());
        b[4..8].copy_from_slice(&0u32.to_le_bytes()); // code: request
        if !mb_call() {
            puts("\rmbox: request failed\r\n");
            return;
        }
        let (rev_off, _) = mb_find(0x0001_0002).unwrap();
        let rev = u32::from_le_bytes(b[rev_off..rev_off + 4].try_into().unwrap());
        let (ser_off, _) = mb_find(0x0001_0003).unwrap();
        let ser = u64::from_le_bytes(b[ser_off..ser_off + 8].try_into().unwrap());
        let (mem_off, _) = mb_find(0x0001_0005).unwrap();
        let base = u32::from_le_bytes(b[mem_off..mem_off + 4].try_into().unwrap());
        let size = u32::from_le_bytes(b[mem_off + 4..mem_off + 8].try_into().unwrap());
        let (clk_off, _) = mb_find(0x0003_0002).unwrap();
        let clk = u32::from_le_bytes(b[clk_off..clk_off + 4].try_into().unwrap());
        puts("\rboard rev:  ");
        putx(rev as u64);
        puts("\r\nserial:     ");
        putx(ser);
        puts("\r\narm memory: 0x");
        putx(base as u64);
        puts(" + 0x");
        putx(size as u64);
        puts("\r\narm clock:  ");
        putu(clk as u64);
        puts(" Hz\r\n");
    }
}


#[no_mangle]
#[unsafe(naked)]
pub extern "C" fn _start() -> ! {
    // host reg-writes are no-ops in this unicorn build; set SP from guest code
    core::arch::naked_asm!(
        "movz w0, #0xfff0",
        "movk w0, #0x3f, lsl #16",
        "mov sp, x0",
        "b rust_main"
    );
}


#[no_mangle]
pub extern "C" fn rust_main() -> ! {

    puts("Hi\n");
    loop {
        puts("> ");
        unsafe { N = 0 };
        loop {
            let c = getc();
            match c {
                13 => break,
                0x7f => unsafe {
                    if N > 0 {
                        N -= 1;
                    }
                },
                c => unsafe {
                    if N < LEN {
                        BUF[N] = c;
                        N += 1;
                    }
                    putc(c);
                },
            }
        }
        let cmd = unsafe { &mut BUF[..N] };
        cmd.make_ascii_lowercase();
        let resp = match &cmd[..] {
            b"hi" => "\rHELLO\r\n",
            b"rpi" => "\rRaspberry Pi 3\r\n",
            b"help" => "\rhi, rpi, help, time, ver, mbox\r\n",
            b"ver" => "\rpi3-emu v1.0\r\n",
            b"time" => "\r", // handled below: prints live timer value
            b"mbox" => "\r", // handled below: VideoCore property request
            b"" => "\r\n",
            _ => "\r?\r\n",
        };
        match &cmd[..] {
            b"time" => {
                puts("\rtime: ");
                putu(unsafe { core::ptr::read_volatile(TMR_CLO as *const u32) } as u64);
                puts(" us\r\n");
            }
            b"mbox" => cmd_mbox(),
            _ => puts(resp),
        }
    }
}
