#![no_std]
#![no_main]

//! Performance benchmark guest: measures MIPS (millions of instructions per
//! second) by running a tight loop and timing it with the BCM2835 system
//! timer (CLO). Reports results for different workload types so the host
//! can compare across configurations.

use pi_runtime::{puts, putx, putu};

const TMR_CLO: u32 = 0x3F00_3004;
const DONE_REG: u32 = 0x3F98_0054;

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

fn elapsed_us(start: u32) -> u32 {
    let now = now_us();
    now.wrapping_sub(start)
}

// Workload 1: tight ALU loop (integer add/multiply)
fn bench_alu(iterations: u32) -> u64 {
    let mut acc: u64 = 0;
    let mut i = 0u32;
    while i < iterations {
        acc = acc.wrapping_add(i as u64);
        acc = acc.wrapping_mul(6364136223846793005);
        i += 1;
    }
    acc
}

// Workload 2: memory access pattern (sequential reads from an array)
fn bench_mem(iterations: u32) -> u64 {
    let mut sum: u64 = 0;
    let buf = [0u64; 1024];
    let mut i = 0u32;
    while i < iterations {
        sum = sum.wrapping_add(buf[(i & 1023) as usize]);
        i += 1;
    }
    sum
}

// Workload 3: branch-heavy (conditional adds)
fn bench_branch(iterations: u32) -> u64 {
    let mut acc: u64 = 0;
    let mut i = 0u32;
    while i < iterations {
        if (i & 1) == 0 {
            acc = acc.wrapping_add(i as u64);
        } else {
            acc = acc.wrapping_sub(i as u64);
        }
        i += 1;
    }
    acc
}

// Workload 4: MMIO read throughput (UART0 flag register — non-destructive)
fn bench_mmio(iterations: u32) -> u64 {
    let uart_fr: *const u32 = 0x3F20_1018 as *const u32;
    let mut count = 0u64;
    let mut i = 0u32;
    while i < iterations {
        let v = unsafe { core::ptr::read_volatile(uart_fr) };
        count = count.wrapping_add(v as u64);
        i += 1;
    }
    count
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
    puts("=== pi3-emu benchmark ===\r\n\r\n");

    let iters = 1_000_000u32;

    // ALU
    puts("[ALU] ");
    putu(iters as u64);
    puts(" iterations\r\n");
    let t0 = now_us();
    let result = bench_alu(iters);
    let us = elapsed_us(t0);
    putu(us as u64);
    puts(" us, result=0x");
    putx(result);
    puts("\r\n");
    if us > 0 {
        puts("  MIPS (insn): ");
        putu((iters as u64 * 1_000_000) / us as u64);
        puts("\r\n");
    }

    // Memory
    puts("\r\n[Memory] ");
    putu(iters as u64);
    puts(" iterations\r\n");
    let t0 = now_us();
    let result = bench_mem(iters);
    let us = elapsed_us(t0);
    putu(us as u64);
    puts(" us, result=0x");
    putx(result);
    puts("\r\n");
    if us > 0 {
        puts("  MIPS (insn): ");
        putu((iters as u64 * 1_000_000) / us as u64);
        puts("\r\n");
    }

    // Branch
    puts("\r\n[Branch] ");
    putu(iters as u64);
    puts(" iterations\r\n");
    let t0 = now_us();
    let result = bench_branch(iters);
    let us = elapsed_us(t0);
    putu(us as u64);
    puts(" us, result=0x");
    putx(result);
    puts("\r\n");
    if us > 0 {
        puts("  MIPS (insn): ");
        putu((iters as u64 * 1_000_000) / us as u64);
        puts("\r\n");
    }

    // MMIO
    let mmio_iters = 100_000u32;
    puts("\r\n[MMIO] ");
    putu(mmio_iters as u64);
    puts(" iterations\r\n");
    let t0 = now_us();
    let result = bench_mmio(mmio_iters);
    let us = elapsed_us(t0);
    putu(us as u64);
    puts(" us, result=0x");
    putx(result);
    puts("\r\n");
    if us > 0 {
        puts("  KIOS (mmio insn/s): ");
        putu((mmio_iters as u64 * 1_000_000) / us as u64);
        puts("\r\n");
    }

    puts("\r\n=== benchmark done ===\r\n");
    mmio_write(DONE_REG, 1);
    loop { core::hint::spin_loop(); }
}
