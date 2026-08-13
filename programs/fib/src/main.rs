#![no_std]
#![no_main]

use pi_runtime::{getc, putc, puts, putu};


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

    puts("fib demo: enter prints fibonacci 0..12\r\n");
    loop {
        let c = getc();
        if c == 13 {
            puts("\r\n");
            let mut a: u64 = 0;
            let mut b: u64 = 1;
            let mut n: u64 = 0;
            while n <= 12 {
                putu(a);
                putc(b' ');
                let t = a + b;
                a = b;
                b = t;
                n += 1;
            }
            puts("\r\n");
        }
    }
}
