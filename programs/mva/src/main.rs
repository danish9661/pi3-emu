#![no_std]
#![no_main]

//! mva: real MMU via the arch sysregs (SCTLR_EL1.M).
//!
//! M12 (host-assisted mmu guest) could not enable translation because the
//! then-current unicorn.js build made the MAIR_EL1/SCTLR_EL1 writes raise
//! UNDEF. The M20 fork rebuild fixed the AArch64 exception paths, so this
//! guest tries the real thing: 4KB-granule stage-1 tables, T0SZ=25,
//! TTBR0_EL1 at 0x280000, MAIR attr0 = normal WB, then SCTLR_EL1 M+C+I.
//!
//!   L0[0] -> L1a, L1a[0] = 1G identity block (VA 0..1GB == PA)
//!   L0[2] -> L1b, L1b[0] = table -> L2, L2[0] = 2M block VA 0x80000000
//!           -> PA 0x200000 (alias)
//!
//! After enabling, the guest keeps executing (identity code path), stores
//! through the alias, and verifies both PAs agree.

use pi_runtime::puts;

#[no_mangle]
pub static mut SCRATCH: [u64; 4] = [0; 4];

const L0: u32 = 0x280000;
const L1A: u32 = 0x281000;
const L1B: u32 = 0x282000;
const L2: u32 = 0x283000;

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
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
    puts("mva: enabling the real MMU (SCTLR_EL1.M) ...\r\n");

    unsafe {
        // Page tables: 4K pages, T0SZ=25 (39-bit VA), inner WB, attr0 = 0xFF.
        // Zero all tables first, then write entries (the L0 loop must not
        // erase L0[2] afterwards). 64-bit descriptors, 8-byte stride.
        for i in 0..512 {
            mmio_write(L2 + 8 * i, 0);
        }
        for i in 0..512 {
            mmio_write(L1A + 8 * i, 0);
        }
        for i in 0..512 {
            mmio_write(L1B + 8 * i, 0);
        }
        for i in 0..512 {
            mmio_write(L0 + 8 * i, 0);
        }

        // Level-1 block descriptors have bits[1:0] = 01 (bit1=1 is a TABLE).
        // TTBR0 IS the level-1 table (start level 1): L0[0] = 1G identity
        // block (VA 0..1G == PA). L0[2] -> L1B with the 2M block directly
        // at level 2 (a 2M block is invalid at level 3, so no L2 table).
        mmio_write(L0, 0x401); // 1G identity block: AF | AP=00 | attr0
        mmio_write(L0 + 16, L1B | 0b11); // L0[2] (VA 0x80000000), 8-byte entries
        mmio_write(L1B, 0x200401); // 2M alias block VA 0x80000000 -> PA 0x200000

        core::arch::asm!("dsb ish");
        core::arch::asm!("isb");

        // TCR_EL1: T0SZ=25, TG0=4K, SH0=inner, IRGN0/ORGN0=WB.
        core::arch::asm!("msr tcr_el1, {0}", in(reg) 0x3519u64);
        // MAIR_EL1: attr0 = normal WB (0xFF).
        core::arch::asm!("msr mair_el1, {0}", in(reg) 0xFFu64);
        core::arch::asm!("msr ttbr0_el1, {0}", in(reg) L0 as u64);
        core::arch::asm!("dsb ish");
        core::arch::asm!("isb");

        // SCTLR_EL1: M (0) | C (2) | I (12).
        let mut sctlr: u64;
        core::arch::asm!("mrs {0}, sctlr_el1", out(reg) sctlr);
        sctlr |= 1 << 0 | 1 << 2 | 1 << 12;
        core::arch::asm!("msr sctlr_el1, {0}", in(reg) sctlr);
        core::arch::asm!("dsb sy");
        core::arch::asm!("isb");
    }

    // Still running with translation on — identity code path.
    mmio_write(0x200000, 0xDEAD_BEEF);
    let pa = mmio_read(0x200000);
    mmio_write(0x8000_0008, 0xCAFE_F00D);
    let va = mmio_read(0x8000_0008);
    let pa2 = mmio_read(0x200008);

    unsafe {
        SCRATCH[0] = pa as u64;
        SCRATCH[1] = va as u64;
        SCRATCH[2] = pa2 as u64;
        SCRATCH[3] = 1;
    }

    if pa == 0xDEAD_BEEF && va == 0xCAFE_F00D && pa2 == 0xCAFE_F00D {
        puts("mva: real MMU: PASS\r\n");
    } else {
        puts("mva: real MMU: FAIL\r\n");
    }
    loop {
        core::hint::spin_loop();
    }
}