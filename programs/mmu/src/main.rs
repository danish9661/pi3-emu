#![no_std]
#![no_main]

//! MMU demo: host-assisted virtual memory.
//!
//! This unicorn build cannot run a guest with SCTLR.M set (the sysreg write
//! raises a CPU exception and MAIR_EL1 is not implemented), so translation is
//! provided by the host (src/mmu.js). The guest still does the real work —
//! it builds a classic 4-level page table in RAM and enables the MMU through
//! the MMU_CTL window at 0x3F00D000 — and the host walks those tables:
//!
//!   VA 0x00000000 - 0x3FFFFFFF  1G block  -> PA 0x00000000 (identity)
//!   VA 0x80000000 - 0x801FFFFF  2M block  -> PA 0x00300000 (alias)
//!
//! The demo stores/loads data through the alias, copies a little function to
//! PA 0x300200 and calls it at VA 0x80000200 (executing from the shadow
//! copy), and verifies every result.

use pi_runtime::{puts, putu};

const UART: *mut u32 = 0x3F20_1000 as *mut u32;
const RX_SLOT: *mut u32 = (0x3F20_1000 + 0x80) as *mut u32;

const MMU_CTL: u32 = 0x3F00_D000;

const L0: u32 = 0x280000; // 4K-aligned table pages (required)
const L1: u32 = 0x281000;
const L2: u32 = 0x282000;

const ALIAS_VA: u32 = 0x8000_0000;
const DATA_PA: u32 = 0x200000; // 2M-aligned (2M block descriptor)

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

// Five words of machine code, copied to PA 0x300200 and called via the alias:
//   movz x0, #0xdead
//   movz x1, #8
//   movk x1, #0x8000, lsl 16
//   str  x0, [x1]      ; [VA 0x80000008] = 0xdead
//   ret
const ALIAS_FN: [u32; 5] = [0xD29B_D5A0, 0xD280_0101, 0xF2B0_0001, 0xF900_0020, 0xD65F_03C0];
const ALIAS_FN_PA: u32 = 0x200200;
const ALIAS_FN_VA: u32 = 0x8000_0200;

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
    let mut ok = true;
    puts("mmu: host-assisted MMU @ 0x3F00D000\r\n");

    // Page tables (4K pages, 48-bit VA, standard descriptors):
    //   L0[0] = table -> L1     L1[0] = 1G block (identity, attr0, AF)
    //   L1[1] = 0               L1[2] = table -> L2     L1[3] = 0
    //   L2[0] = 2M block VA 0x80000000 -> PA 0x200000   L2[1..] = 0
    unsafe {
        mmio_write(L0, L1 | 0b01);
        mmio_write(L1, 0x403); // 1G identity block
        mmio_write(L1 + 4, 0);
        mmio_write(L1 + 16, L2 | 0b01); // L1[2]
        mmio_write(L1 + 12, 0);
        mmio_write(L2, 0x200403); // 2M alias block
        for i in 1..512 {
            mmio_write(L2 + 4 * i, 0);
        }
    }
    puts("mmu: tables at 0x280000, enabling...\r\n");
    mmio_write(MMU_CTL, 0x280001); // root | 1 = enable
    // The host enables translation between slices; wait for the reflected
    // status so the alias is live before we touch it (same pattern as getc).
    let mut spins = 0;
    while (mmio_read(MMU_CTL) & 1) == 0 {
        spins += 1;
        if spins > 200_000 {
            break;
        }
    }
    puts("mmu: enabled\r\n");

    // 1: store through the alias, read back through the identity map.
    mmio_write(ALIAS_VA, 0x5a5a);
    let a = mmio_read(DATA_PA) == 0x5a5a;
    puts(if a {
        "mmu: alias write -> PA read OK\r\n"
    } else {
        "mmu: alias write -> PA read FAIL\r\n"
    });
    ok &= a;

    // 2: store through the identity map, read back through the alias.
    mmio_write(DATA_PA + 4, 0xbeef);
    let b = mmio_read(ALIAS_VA + 4) == 0xbeef;
    puts(if b {
        "mmu: PA write -> alias read OK\r\n"
    } else {
        "mmu: PA write -> alias read FAIL\r\n"
    });
    ok &= b;

    // 3: copy a function to PA 0x300200 and call it at VA 0x80000200;
    // the host mirror keeps the shadow copy live, so it executes there.
    for (i, w) in ALIAS_FN.iter().enumerate() {
        mmio_write(ALIAS_FN_PA + 4 * i as u32, *w);
    }
    let f: extern "C" fn() = unsafe { core::mem::transmute(ALIAS_FN_VA as usize) };
    f();
    let c = mmio_read(DATA_PA + 8) == 0xdead;
    puts(if c {
        "mmu: shadow-code call OK\r\n"
    } else {
        "mmu: shadow-code call FAIL\r\n"
    });
    ok &= c;

    puts(if ok {
        "mmu: all checks passed\r\n"
    } else {
        "mmu: FAILED\r\n"
    });
    puts("mmu: parked\r\n");
    mmio_write(MMU_CTL + 4, 1); // MMU_DONE: host extension, like TMR_DONE

    // The UART TX slots are drained by the host after each slice; parking
    // lets the done-driven driver return. No IRQs here, so nothing else to do.
    loop {
        core::hint::spin_loop();
    }
}