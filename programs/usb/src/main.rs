#![no_std]
#![no_main]

//! DWC2 OTG bring-up demo: the Synopsys DesignWare USB 2.0 OTG controller
//! at 0x3F980000 (QEMU hcd-dwc2 reset values + Linux dwc2/hw.h bit defs).
//! The guest walks the real driver probe sequence a dwc2 host stack
//! performs: ID/config reads, core soft reset, GAHBCFG global-interrupt
//! enable, HPRT power + port reset, and one control transfer
//! (GET_DESCRIPTOR device) on host channel 0 with XFERCOMPL completion.
//! Parks on USB DONE (+0xFF0), like periphs/debug.

use pi_runtime::{puts, putx};

const USB: u32 = 0x3F98_0000;
const GOTGCTL: u32 = USB + 0x000;
const GAHBCFG: u32 = USB + 0x008;
const GRSTCTL: u32 = USB + 0x010;
const GINTSTS: u32 = USB + 0x014;
const GINTMSK: u32 = USB + 0x018;
const GRXFSIZ: u32 = USB + 0x024;
const GNPTXFSIZ: u32 = USB + 0x028;
const GSNPSID: u32 = USB + 0x040;
const GHWCFG2: u32 = USB + 0x048;
const GHWCFG3: u32 = USB + 0x04C;
const HCFG: u32 = USB + 0x400;
const HFNUM: u32 = USB + 0x408;
const HPRT: u32 = USB + 0x440;
const HCCHAR0: u32 = USB + 0x500;
const HCINT0: u32 = USB + 0x508;
const HCINTMSK0: u32 = USB + 0x50C;
const HCTSIZ0: u32 = USB + 0x510;
const HCDMA0: u32 = USB + 0x514;
const USB_DONE: u32 = USB + 0xFF0;

const GAHBCFG_GLBL_INTR_EN: u32 = 1 << 0;
const GRSTCTL_CSFTRST: u32 = 1 << 0;
const GRSTCTL_AHBIDLE: u32 = 1 << 31;
const GINTSTS_CURMODE_HOST: u32 = 1 << 0;
const HPRT_PPWR: u32 = 1 << 12;
const HPRT_PRTRST: u32 = 1 << 8;
const HPRT_PRTENA: u32 = 1 << 2;
const HPRT_CONNSTS: u32 = 1 << 0;
const HCCHAR_CHENA: u32 = 1 << 31;
const HCCHAR_EPDIR_IN: u32 = 1 << 15;
const HCINT_XFERCOMPL: u32 = 1 << 0;
const HCINT_CHHLTD: u32 = 1 << 1;

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

static mut SETUP_BUF: [u8; 64] = [0; 64];
static mut DATA_BUF: [u8; 64] = [0; 64];

// Byte stores (no memset/vectorized clear — pi-cpu faults SIMD
// mov.h; memset on DATA_BUF lowers to dup/mov.h stores).
fn zero64(slot: *mut u8) {
    for i in 0..64 {
        unsafe { core::ptr::write_volatile(slot.add(i), 0) };
    }
}

fn check(name: &str, got: u32, want: u32) -> bool {
    puts("usb: ");
    puts(name);
    puts(" = 0x");
    putx(got as u64);
    if got == want {
        puts(" [OK]\r\n");
        true
    } else {
        puts(" [FAIL want 0x");
        putx(want as u64);
        puts("]\r\n");
        false
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
    puts("usb: DWC2 OTG @ 0x3F980000\r\n");
    let mut pass = 0u32;
    let mut fail = 0u32;

    // --- ID/config reads (QEMU hcd-dwc2 reset values) ---
    let snpsid = mmio_read(GSNPSID);
    if snpsid == 0x4F54_294A || snpsid == 0x4F54_280A {
        puts("usb: GSNPSID OK (0x");
        putx(snpsid as u64);
        puts(")\r\n");
        pass += 1;
    } else {
        puts("usb: GSNPSID FAIL (0x");
        putx(snpsid as u64);
        puts(")\r\n");
        fail += 1;
    }
    // Machine-checked reset values (QEMU hcd-dwc2.c dwc2_reset_enter:
    // GHWCFG2 = 0x250dc016, GHWCFG3 = 0x10000044 — python3 from the
    // shift expression, never hand-hex).
    if check("GHWCFG2", mmio_read(GHWCFG2), 0x250D_C016) { pass += 1; } else { fail += 1; }
    if check("GHWCFG3", mmio_read(GHWCFG3), 0x1000_0044) { pass += 1; } else { fail += 1; }
    if check("GRXFSIZ", mmio_read(GRXFSIZ), 1024) { pass += 1; } else { fail += 1; }
    if check("GNPTXFSIZ", mmio_read(GNPTXFSIZ), 1024 << 16) { pass += 1; } else { fail += 1; }
    let gotgctl = mmio_read(GOTGCTL);
    if gotgctl & 0x000D0000 == 0x000D0000 {
        puts("usb: GOTGCTL session-valid OK\r\n");
        pass += 1;
    } else {
        puts("usb: GOTGCTL FAIL (0x");
        putx(gotgctl as u64);
        puts(")\r\n");
        fail += 1;
    }

    // --- core soft reset (CSFTRST self-clears, AHBIDLE stays) ---
    mmio_write(GRSTCTL, GRSTCTL_CSFTRST);
    let mut spins = 0;
    while mmio_read(GRSTCTL) & GRSTCTL_CSFTRST != 0 {
        spins += 1;
        if spins > 0x40000 { break; }
    }
    if mmio_read(GRSTCTL) & GRSTCTL_AHBIDLE != 0 {
        puts("usb: GRSTCTL soft-reset OK\r\n");
        pass += 1;
    } else {
        puts("usb: GRSTCTL FAIL\r\n");
        fail += 1;
    }

    // --- host mode: global-interrupt enable + HPRT power/reset ---
    mmio_write(GAHBCFG, GAHBCFG_GLBL_INTR_EN);
    mmio_write(HPRT, HPRT_PPWR);
    let mut spins = 0;
    while mmio_read(HPRT) & HPRT_CONNSTS == 0 {
        spins += 1;
        if spins > 0x40000 { break; }
    }
    if mmio_read(HPRT) & HPRT_CONNSTS != 0 {
        puts("usb: HPRT connect OK\r\n");
        pass += 1;
    } else {
        puts("usb: HPRT FAIL\r\n");
        fail += 1;
    }
    mmio_write(HPRT, mmio_read(HPRT) | HPRT_PRTRST);
    let mut spins = 0;
    while mmio_read(HPRT) & HPRT_PRTENA == 0 {
        spins += 1;
        if spins > 0x40000 { break; }
        if spins == 0x1000 {
            mmio_write(HPRT, mmio_read(HPRT) & !HPRT_PRTRST);
        }
    }
    if mmio_read(HPRT) & HPRT_PRTENA != 0 {
        puts("usb: HPRT reset+enable OK\r\n");
        pass += 1;
    } else {
        puts("usb: HPRT reset FAIL\r\n");
        fail += 1;
    }

    // --- HFNUM frame counter advances ---
    let f0 = mmio_read(HFNUM) & 0x3fff;
    let mut f1 = f0;
    for _ in 0..1000 {
        f1 = mmio_read(HFNUM) & 0x3fff;
        if f1 != f0 { break; }
        core::hint::spin_loop();
    }
    if f1 != f0 {
        puts("usb: HFNUM ticking OK\r\n");
        pass += 1;
    } else {
        puts("usb: HFNUM FAIL\r\n");
        fail += 1;
    }

    // --- control transfer: GET_DESCRIPTOR(device) on channel 0 ---
    // Setup packet: GET_DESCRIPTOR device, wLength 18.
    unsafe {
        SETUP_BUF[0] = 0x80;
        SETUP_BUF[1] = 6;
        SETUP_BUF[2] = 0;
        SETUP_BUF[3] = 1;
        SETUP_BUF[4] = 0;
        SETUP_BUF[5] = 0;
        SETUP_BUF[6] = 18;
        SETUP_BUF[7] = 0;
    }
    zero64(unsafe { (&raw mut DATA_BUF) as *mut u8 });
    let setup_pa = unsafe { (&raw const SETUP_BUF) as u32 };
    let data_pa = unsafe { (&raw const DATA_BUF) as u32 };
    // SETUP stage (QEMU: setup moves through the FIFO, not DMA — the
    // model latches these 8 bytes as the pending request): EP0 OUT,
    // HCTSIZ=8, HCDMA=setup buffer.
    mmio_write(HCDMA0, setup_pa);
    mmio_write(HCTSIZ0, 8);
    mmio_write(HCINTMSK0, HCINT_XFERCOMPL | HCINT_CHHLTD);
    mmio_write(HCCHAR0, HCCHAR_CHENA | (64 & 0x7ff));
    let mut spins = 0;
    while mmio_read(HCINT0) & HCINT_XFERCOMPL == 0 {
        spins += 1;
        if spins > 0x40000 { break; }
    }
    mmio_write(HCINT0, HCINT_XFERCOMPL | HCINT_CHHLTD);
    // DATA stage: EP0 IN, HCTSIZ=18, HCDMA=data buffer. Executes the
    // latched GET_DESCRIPTOR and DMAs the 18 descriptor bytes.
    mmio_write(HCDMA0, data_pa);
    mmio_write(HCTSIZ0, 18);
    mmio_write(HCINTMSK0, HCINT_XFERCOMPL | HCINT_CHHLTD);
    // IN, EP0, addr 0, MPS 64, CHENA.
    mmio_write(HCCHAR0, HCCHAR_CHENA | HCCHAR_EPDIR_IN | (64 & 0x7ff));
    let mut spins = 0;
    while mmio_read(HCINT0) & HCINT_XFERCOMPL == 0 {
        spins += 1;
        if spins > 0x40000 { break; }
    }
    let hcint = mmio_read(HCINT0);
    if hcint & HCINT_XFERCOMPL != 0 {
        puts("usb: HCINT XFERCOMPL OK\r\n");
        pass += 1;
    } else {
        puts("usb: HCINT FAIL (0x");
        putx(hcint as u64);
        puts(")\r\n");
        fail += 1;
    }
    // Descriptor bytes: bLength 18, type 1, USB 2.1, vendor 0424, product 7800.
    let ok = unsafe {
        DATA_BUF[0] == 18 && DATA_BUF[1] == 1 && DATA_BUF[8] == 0x24
            && DATA_BUF[9] == 0x04 && DATA_BUF[10] == 0x00 && DATA_BUF[11] == 0x78
    };
    if ok {
        puts("usb: device descriptor OK (0424:7800 LAN7800)\r\n");
        pass += 1;
    } else {
        puts("usb: descriptor FAIL\r\n");
        fail += 1;
    }
    // W1C: clear the completion bits.
    mmio_write(HCINT0, HCINT_XFERCOMPL | HCINT_CHHLTD);
    if mmio_read(HCINT0) & (HCINT_XFERCOMPL | HCINT_CHHLTD) == 0 {
        puts("usb: HCINT W1C OK\r\n");
        pass += 1;
    } else {
        puts("usb: HCINT W1C FAIL\r\n");
        fail += 1;
    }
    // GINTSTS still reports host mode.
    if mmio_read(GINTSTS) & GINTSTS_CURMODE_HOST != 0 {
        puts("usb: CURMODE_HOST OK\r\n");
        pass += 1;
    } else {
        puts("usb: CURMODE FAIL\r\n");
        fail += 1;
    }
    let _ = HCFG;

    puts("usb: ");
    putx(pass as u64);
    puts(" pass\r\n");
    if fail == 0 {
        puts("usb: ALL PASS\r\n");
    } else {
        puts("usb: FAIL\r\n");
    }
    mmio_write(USB_DONE, 1);
    loop {
        core::hint::spin_loop();
    }
}
