#![no_std]
#![no_main]

use pi_runtime::{getc, putc, puts};

const LEN: usize = 64;

static mut BUF: [u8; LEN] = [0; LEN];
static mut N: usize = 0;


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
            b"help" => "\rhi, rpi, help, ver\r\n",
            b"ver" => "\rpi3-emu v1.0\r\n",
            b"" => "\r\n",
            _ => "\r?\r\n",
        };
        puts(resp);
    }
}
