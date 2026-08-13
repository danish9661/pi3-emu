#![no_std]
#![no_main]

use pi_runtime::{getc, puts, putu};


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

    puts("sum demo: enter adds 1..10\r\n");
    loop {
        let c = getc();
        if c == 13 {
            let mut total: u64 = 0;
            let mut i: u64 = 1;
            while i <= 10 {
                total += i;
                i += 1;
            }
            puts("\r\nsum(1..10) = ");
            putu(total);
            puts("\r\n");
        } else if c == 0x7f || c == 0 {
            // ignore
        }
    }
}
