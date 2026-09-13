// SPDX-License-Identifier: MIT OR Apache-2.0
//! M54 kernel: driver structure + sync SVC + timer IRQ + identity MMU,
//! in the rust-raspberrypi-OS shape (dep-free: no tock-registers, stable).
//!
//! _start checks CurrentEL==EL2 (0x8) and MPIDR core 0, zeroes .bss,
//! requires CNTFRQ_EL0≠0, then rust_main(stack_top) brings up the board
//! drivers (GPIO pins 14/15 to ALT0, PL011 init), installs the exception
//! vectors, drops to EL1 via SPSR_EL2/ELR_EL2/SP_EL1 + eret, and kernel_el1
//! runs the bring-up demos: SVC round-trip, MMU on, 3 timer ticks, echo.

#![no_std]
#![no_main]

mod bsp;
mod console;
mod driver;
mod print;
mod synchronization;

use console::interface::Read;
use core::panic::PanicInfo;

unsafe extern "C" {
    static mut __bss_start: u64;
    static mut __bss_end: u64;
}

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

#[inline(always)]
fn mrs_currentel() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {0}, CurrentEL", out(reg) v, options(nostack)) };
    v
}

#[inline(always)]
fn mrs_cntfrq() -> u32 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {0}, CNTFRQ_EL0", out(reg) v, options(nostack)) };
    v as u32
}

#[inline(always)]
fn mrs_cntpct() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {0}, CNTPCT_EL0", out(reg) v, options(nostack)) };
    v
}

#[inline(always)]
fn mrs_esr() -> u64 {
    let v: u64;
    unsafe { core::arch::asm!("mrs {0}, ESR_EL1", out(reg) v, options(nostack)) };
    v
}

/// 09_privilege_level shape: prepare the EL2->EL1 transition, then eret.
/// CNTHCTL/CNTVOFF/HCR writes are absorbed by pi-cpu (no trap model yet).
#[inline(always)]
unsafe fn drop_to_el1(stack_top: u64) {
    unsafe {
        core::arch::asm!(
            "msr CNTHCTL_EL2, xzr", // EL1 access to timers (absorbed)
            "msr CNTVOFF_EL2, xzr", // no counter offset (absorbed)
            "mov x9, #1",
            "lsl x9, x9, #31",      // HCR_EL2.RW = EL1 is AArch64 (absorbed)
            "msr HCR_EL2, x9",
            "mov x9, #0x3c5",       // D/A/I/F masked + M = EL1h (SP_EL1)
            "msr SPSR_EL2, x9",
            "adr x9, kernel_el1",
            "msr ELR_EL2, x9",
            "mov x9, {0}",
            "msr SP_EL1, x9",
            "eret",
            in(reg) stack_top,
            out("x9") _,
            options(nostack),
        );
    }
}

fn install_vectors() {
    unsafe {
        core::arch::asm!(
            "adr x0, vec_start",
            "msr vbar_el1, x0",
            "isb",
            out("x0") _,
            options(nostack),
        );
    }
}

fn arm_timer(freq: u32, ctl: u32) {
    unsafe {
        core::arch::asm!(
            "msr CNTP_TVAL_EL0, {0}",
            "msr CNTP_CTL_EL0, {1}",
            in(reg) freq as u64,
            in(reg) ctl as u64,
            options(nostack),
        );
    }
}

/// 4K-granule identity map (mva-guest shape, NOT the tutorial's 64K):
/// L1[0] = 1G block VA 0..1G == PA (covers RAM + all MMIO windows, which
/// additionally bypass translation in pi-cpu). T0SZ=25 (39-bit VA),
/// TTBR0 at 0x280000 (pinned by kernel.ld .tables), MAIR attr0 normal.
/// Zero the 512-entry L1 at 0x280000 in pure asm (x9-x11 only):
/// a Rust loop keeps the base/limit in caller-saved registers that the
/// "mmu: off" print (a full fmt::write call) clobbers — first cut died
/// in Translation at the TTBR0 write with L1-base garbage.
#[unsafe(naked)]
extern "C" fn asm_clear_l1() {
    core::arch::naked_asm!(
        "movz x9, #0x8000",
        "movk x9, #0x28, lsl #16", // x9 = 0x280000
        "movz x10, #0x9000",
        "movk x10, #0x28, lsl #16", // x10 = 0x281000 (end)
        "0:",
        "cmp x9, x10",
        "b.hs 1f",
        "str xzr, [x9], #8",
        "b 0b",
        "1:",
        "ret",
    )
}

fn mmu_on() {
    unsafe {
        asm_clear_l1();
        // L1[0]: 1G identity block, bits[1:0]=01, AF set.
        core::ptr::write_volatile(0x280000 as *mut u64, 0x401);
        core::arch::asm!("dsb ish", "isb", options(nostack));
        core::arch::asm!("msr TCR_EL1, {0}", in(reg) 0x3519u64, options(nostack));
        core::arch::asm!("msr MAIR_EL1, {0}", in(reg) 0xFFu64, options(nostack));
        core::arch::asm!("msr TTBR0_EL1, {0}", in(reg) 0x280000u64, options(nostack));
        core::arch::asm!("dsb ish", "isb", options(nostack));
        let mut sctlr: u64;
        core::arch::asm!("mrs {0}, SCTLR_EL1", out(reg) sctlr, options(nostack));
        sctlr |= 1 | (1 << 2) | (1 << 12); // M|C|I like mva (C/I ignored)
        core::arch::asm!("msr SCTLR_EL1, {0}", in(reg) sctlr, options(nostack));
        core::arch::asm!("dsb sy", "isb", options(nostack));
    }
}

// Vector table + glue (native-eret shape like the lirq guest; the UART0
// guest's host-assisted IRQ_RET magic is NOT used on the timer path).
// Only the EL1h sync (0x200) and EL1h IRQ (0x280) entries are live.
core::arch::global_asm!(
    ".section .vectors,\"ax\",@progbits",
    ".balign 2048",
    "vec_start:",
    "  b .", // 0x000 SP0 sync
    "  .org 0x080",
    "  b .", // 0x080 SP0 irq
    "  .org 0x100",
    "  b .", // 0x100 SP0 fiq
    "  .org 0x180",
    "  b .", // 0x180 SP0 err
    "  .org 0x200",
    "  b sync_glue", // 0x200 EL1h sync <- SVC lands here
    "  .org 0x280",
    "  b irq_glue", // 0x280 EL1h irq <- arch-timer lands here
    "  .org 0x300",
    "  b .", // 0x300 EL1h fiq
    "  .org 0x380",
    "  b .", // 0x380 EL1h err
    "  .org 0x400",
    "  b .", // 0x400 a64 sync
    "  .org 0x480",
    "  b .", // 0x480 a64 irq
    "  .org 0x500",
    "  b .", // 0x500 a64 fiq
    "  .org 0x580",
    "  b .", // 0x580 a64 err
    "  .org 0x600",
    "  b .", // 0x600 a32 sync
    "  .org 0x680",
    "  b .", // 0x680 a32 irq
    "  .org 0x700",
    "  b .", // 0x700 a32 fiq
    "  .org 0x780",
    "  b .", // 0x780 a32 err
    "  .org 0x800",
    "sync_glue:",
    "  stp x0, x1, [sp, #-16]!",
    "  mrs x0, ELR_EL1",
    "  mrs x1, SPSR_EL1",
    "  mrs x2, ESR_EL1",
    "  stp x2, xzr, [sp, #-16]!",
    "  bl svc_handler",
    "  ldp x2, xzr, [sp], #16",
    "  mrs x0, ELR_EL1",
    "  add x0, x0, #4", // skip the faulting svc
    "  msr ELR_EL1, x0",
    "  ldp x0, x1, [sp], #16",
    "  eret",
    "irq_glue:",
    "  stp x29, x30, [sp, #-16]!",
    "  stp x0, x1, [sp, #-16]!",
    "  bl timer_handler",
    "  ldp x0, x1, [sp], #16",
    "  ldp x29, x30, [sp], #16",
    "  msr DAIFSet, #2",
    "  eret",
);

static mut TIMER_COUNT: u64 = 0;
static mut TIMER_DONE: u64 = 0;

#[no_mangle]
pub extern "C" fn svc_handler(elr: u64, _spsr: u64, esr: u64) {
    let ec = (esr >> 26) & 0x3f;
    let iss = esr & 0xffffff;
    print!("rpi-kernel: SVC EC 0x{:x} ISS 0x{:x} ELR 0x{:x}\r\n", ec, iss, elr);
}

#[no_mangle]
pub extern "C" fn timer_handler() {
    // Expect CNTPNS only in the local-block source (GPU bit8 clear).
    let src = mmio_read(0x4000_0060);
    unsafe {
        // Counter via absolute addresses only (adr, never adrp): the
        // handler runs with the MMU on, and rustc's adrp for these
        // statics mis-forms post-enable (UnmappedData 0x124F810 —
        // page part of a pre-enable PC baked at link time). adr is
        // PC-relative ±1 MB and always exact.
        let n: u64;
        core::arch::asm!(
            "adr x9, {TC}",
            "ldr x9, [x9]",
            "add x9, x9, #1",
            "adr x10, {TC}",
            "str x9, [x10]",
            "mov {n}, x9",
            TC = sym TIMER_COUNT,
            n = out(reg) n,
            out("x9") _,
            out("x10") _,
            options(nostack),
        );
        print!("rpi-kernel: [timer {}] src 0x{:x}\r\n", n, src);
        if n < 3 {
            let freq = mrs_cntfrq();
            arm_timer(freq, 1); // re-arm 1 s (TVAL + CTL enable)
        } else {
            arm_timer(0, 0); // CTL=0 de-asserts the line (no re-entry)
            core::arch::asm!(
                "mov x9, #1",
                "adr x10, {TD}",
                "str x9, [x10]",
                TD = sym TIMER_DONE,
                out("x9") _,
                out("x10") _,
                options(nostack),
            );
        }
    }
}

#[no_mangle]
#[unsafe(naked)]
pub extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        // 09 boot.s shape: only EL2 proceeds (CurrentEL == 0x8, i.e. EL2).
        "mrs x0, CurrentEL",
        "cmp x0, #0x8",
        "b.ne 9f",
        // Only the boot core proceeds (MPIDR core id == 0).
        "mrs x1, MPIDR_EL1",
        "and x1, x1, #0x3",
        "cbnz x1, 9f",
        // Stack at the top of the 4 MB RAM.
        "movz x9, #0xfff0",
        "movk x9, #0x3f, lsl #16",
        "mov sp, x9",
        // Zero .bss.
        "adr x9, __bss_start",
        "adr x10, __bss_end",
        "0:",
        "cmp x9, x10",
        "b.hs 1f",
        "str xzr, [x9], #8",
        "b 0b",
        "1:",
        // CNTFRQ must read non-zero (09 aborts to the park loop on 0).
        "mrs x2, CNTFRQ_EL0",
        "cbz x2, 9f",
        // x0 = stack top for the EL2->EL1 drop; rust_main never returns.
        "mov x0, sp",
        "b rust_main",
        // Park: wfe is a no-op in pi-cpu, so spin plainly.
        "9:",
        "b 9b",
    )
}

#[no_mangle]
pub extern "C" fn kernel_el1() -> ! {
    use console::interface::Read;

    // Now in EL1 after the eret drop.
    print!("rpi-kernel M54: in EL1 (CurrentEL 0x{:x})\r\n", mrs_currentel());
    let freq = mrs_cntfrq();
    print!("rpi-kernel: timer freq {} Hz\r\n", freq);
    print!("rpi-kernel: drivers loaded:\r\n");
    // NOTE: manager().enumerate() is deliberately NOT called here: the
    // tutorial's enumerate walks the NullLock descriptor array with
    // `adr x25, MANAGER`-relative addressing that the current linker
    // script places out of range — parked for the M54-follow-up, GPIO +
    // UART init above is what matters (console works, proven by every
    // line above).

    // SVC round-trip demo (ch12 shape): handler prints EC/ISS/ELR.
    print!("rpi-kernel: before svc\r\n");
    unsafe { core::arch::asm!("svc #0x1337", options(nostack)) };
    print!("rpi-kernel: after svc (ESR 0x{:x})\r\n", mrs_esr());

    // Identity MMU on (mva-guest 4K shape; MMIO windows bypass).
    // NOTE: rustc keeps the MSB (w8=0x280000) in a caller register
    // across the "mmu: off" print, so the print call's own frame
    // writes (str xN,[sp,#..]) must not clobber it — recompute the
    // base AFTER the print (first cut computed before and died in
    // Translation at the TTBR0 write with L1-base garbage).
    print!("rpi-kernel: mmu: off\r\n");
    mmu_on();
    print!("rpi-kernel: mmu: on\r\n");

    // 3 timer ticks via CNTP_TVAL + native-er et handler (lirq shape).
    let freq = mrs_cntfrq();
    arm_timer(freq, 1);
    unsafe {
        core::arch::asm!("msr DAIFClr, #2", options(nostack));
        // Poll TIMER_DONE with adr (never adrp): same post-MMU address
        // formation rule as the handler (adrp mis-forms, adr is exact).
        loop {
            let done: u64;
            core::arch::asm!(
                "adr x9, {TD}",
                "ldr {d}, [x9]",
                TD = sym TIMER_DONE,
                d = out(reg) done,
                out("x9") _,
                options(nostack),
            );
            if done == 1 {
                break;
            }
            core::arch::asm!("msr DAIFClr, #2", options(nostack));
            core::hint::spin_loop();
        }
    }
    print!("rpi-kernel: timer done\r\n");

    // kernel_main's tail in 09: echo input.
    print!("rpi-kernel: Echoing input now\r\n");
    console::console().clear_rx();
    loop {
        let c = console::console().read_char();
        print!("rpi-kernel: [echo '{}']\r\n", c);
    }
}

#[no_mangle]
pub extern "C" fn rust_main(stack_top: u64) -> ! {
    // Console must exist before the first print!: init_drivers registers
    // the UART console (printing before registration panics in expect()
    // and parks the core in the panic spin — silent hang, fault null).
    unsafe { crate::bsp::driver::init_drivers() };
    install_vectors();
    print!("rpi-kernel M54: hello from 0x80000 (EL2)\r\n");
    print!("rpi-kernel: VBAR installed\r\n");
    unsafe { drop_to_el1(stack_top) };
    loop {
        core::hint::spin_loop();
    }
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    // Raw UART (never the registered console — the panic may BE a
    // missing console), then park. Silent spins cost hours.
    unsafe {
        const UART0: *mut u32 = 0x3F20_1000 as *mut u32;
        for b in b"rpi-kernel: PANIC\r\n" {
            for _ in 0..2000 {
                if core::ptr::read_volatile(UART0.add(0x18 / 4)) & (1 << 5) == 0 {
                    break;
                }
                core::hint::spin_loop();
            }
            core::ptr::write_volatile(UART0, *b as u32);
        }
    }
    loop {
        core::hint::spin_loop();
    }
}
