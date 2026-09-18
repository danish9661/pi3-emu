#![no_std]
#![no_main]

//! LAN7800 ethernet demo: the Pi 3 B+ onboard NIC (usb424:7800) behind
//! the DWC2 OTG host at 0x3F980000. The guest brings the core up (like
//! the usb guest: reset, GAHBCFG, HPRT power/reset), then runs the
//! ethernet data path the lan78xx driver uses:
//!   1. bulk-OUT on EP2 (host -> LAN7800): transmit one frame; the
//!      model queues it to the harness TX path (usb_tx_take).
//!   2. bulk-IN on EP1 (LAN7800 -> host): receive one frame. The model
//!      loopbacks the transmitted frame (like a link-partner echo), so
//!      the guest verifies TX bytes == RX bytes.
//!   3. IRQ path: GINTMSK.HCHINT + HAINTMSK[0] armed, so the channel
//!      completion raises GINTSTS.HCHINT and legacy-IC PENDING1 bit 9
//!      (INTERRUPT_USB = GPU IRQ 9); the guest reads both before W1C.
//! Parks on USB DONE (+0xFF0), like periphs/debug/usb.

use pi_runtime::{puts, putx};

const USB: u32 = 0x3F98_0000;
const GAHBCFG: u32 = USB + 0x008;
const GRSTCTL: u32 = USB + 0x010;
const GINTSTS: u32 = USB + 0x014;
const GINTMSK: u32 = USB + 0x018;
const HPRT: u32 = USB + 0x440;
const HAINT: u32 = USB + 0x414;
const HAINTMSK: u32 = USB + 0x418;
const HCCHAR0: u32 = USB + 0x500;
const HCINT0: u32 = USB + 0x508;
const HCINTMSK0: u32 = USB + 0x50C;
const HCTSIZ0: u32 = USB + 0x510;
const HCDMA0: u32 = USB + 0x514;
const USB_DONE: u32 = USB + 0xFF0;

const IC_BASE: u32 = 0x3F00_B200;
const IC_PENDING1: u32 = IC_BASE + 0x04;
const IC_ENABLE_IRQS1: u32 = IC_BASE + 0x10;

const GAHBCFG_GLBL_INTR_EN: u32 = 1 << 0;
const GRSTCTL_CSFTRST: u32 = 1 << 0;
const GINTSTS_HCHINT: u32 = 1 << 25;
const HPRT_PPWR: u32 = 1 << 12;
const HPRT_PRTRST: u32 = 1 << 8;
const HPRT_PRTENA: u32 = 1 << 2;
const HPRT_CONNSTS: u32 = 1 << 0;
const HCCHAR_CHENA: u32 = 1 << 31;
const HCCHAR_EPDIR_IN: u32 = 1 << 15;
const HCINT_XFERCOMPL: u32 = 1 << 0;
const HCINT_CHHLTD: u32 = 1 << 1;
const IRQ_USB: u32 = 1 << 9; // INTERRUPT_USB = GPU IRQ 9 (bank-1 bit 9)

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
}

// Bulk endpoint programs (lan78xx: EP1 IN + EP2 OUT, MPS 512).
fn hcchar(ep: u32, dir_in: bool) -> u32 {
    HCCHAR_CHENA | (if dir_in { HCCHAR_EPDIR_IN } else { 0 })
        | ((ep & 0xf) << 11) | (512 & 0x7ff)
}

static mut TX_BUF: [u8; 1536] = [0; 1536];
static mut RX_BUF: [u8; 1536] = [0; 1536];

// Byte stores (no memset/vectorized clear — pi-cpu faults SIMD
// mov.h; memset on RX_BUF lowers to dup/mov.h stores).
fn zero1536(slot: *mut u8) {
    for i in 0..1536 {
        unsafe { core::ptr::write_volatile(slot.add(i), 0) };
    }
}

fn wait_hcint() -> u32 {
    let mut spins = 0;
    loop {
        let v = mmio_read(HCINT0);
        if v & HCINT_XFERCOMPL != 0 {
            return v;
        }
        spins += 1;
        if spins > 0x40000 {
            return v;
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
    puts("eth: LAN7800 @ DWC2 0x3F980000\r\n");
    let mut pass = 0u32;
    let mut fail = 0u32;

    // --- core bring-up (same as the usb guest) ---
    mmio_write(GRSTCTL, GRSTCTL_CSFTRST);
    let mut spins = 0;
    while mmio_read(GRSTCTL) & GRSTCTL_CSFTRST != 0 {
        spins += 1;
        if spins > 0x40000 { break; }
    }
    mmio_write(GAHBCFG, GAHBCFG_GLBL_INTR_EN);
    mmio_write(HPRT, HPRT_PPWR);
    let mut spins = 0;
    while mmio_read(HPRT) & HPRT_CONNSTS == 0 {
        spins += 1;
        if spins > 0x40000 { break; }
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
        puts("eth: port up OK\r\n");
        pass += 1;
    } else {
        puts("eth: port FAIL\r\n");
        fail += 1;
    }

    // --- IRQ arm: HCHINT mask + channel-0 HAINT + bank-1 bit 9 ---
    mmio_write(GINTMSK, GINTSTS_HCHINT);
    mmio_write(HAINTMSK, 1);
    mmio_write(IC_ENABLE_IRQS1, IRQ_USB);

    // --- TX: one ethernet frame (dst/src/type + payload) via EP2 OUT ---
    // Frame: broadcast dst, our MAC src (b8:27:eb:de:ad:be), ethertype
    // 0x0800, payload "hello lan7800".
    let payload = b"hello lan7800";
    let framelen = 14 + payload.len();
    unsafe {
        TX_BUF[0..6].copy_from_slice(&[0xff; 6]);
        TX_BUF[6..12].copy_from_slice(&[0xb8, 0x27, 0xeb, 0xde, 0xad, 0xbe]);
        TX_BUF[12] = 0x08;
        TX_BUF[13] = 0x00;
        TX_BUF[14..14 + payload.len()].copy_from_slice(payload);
    }
    zero1536(unsafe { (&raw mut RX_BUF) as *mut u8 });
    let tx_pa = unsafe { (&raw const TX_BUF) as u32 };
    let rx_pa = unsafe { (&raw const RX_BUF) as u32 };
    mmio_write(HCDMA0, tx_pa);
    mmio_write(HCTSIZ0, framelen as u32);
    mmio_write(HCINTMSK0, HCINT_XFERCOMPL | HCINT_CHHLTD);
    mmio_write(HCCHAR0, hcchar(2, false));
    let hcint = wait_hcint();
    if hcint & HCINT_XFERCOMPL != 0 {
        puts("eth: TX XFERCOMPL OK\r\n");
        pass += 1;
    } else {
        puts("eth: TX FAIL (0x");
        putx(hcint as u64);
        puts(")\r\n");
        fail += 1;
    }
    mmio_write(HCINT0, HCINT_XFERCOMPL | HCINT_CHHLTD);

    // --- IRQ check: HCHINT + HAINT[0] + PENDING1 bit 9 all live ---
    let gintsts = mmio_read(GINTSTS);
    let haint = mmio_read(HAINT);
    let pend1 = mmio_read(IC_PENDING1);
    if gintsts & GINTSTS_HCHINT != 0 {
        puts("eth: GINTSTS HCHINT OK\r\n");
        pass += 1;
    } else {
        puts("eth: HCHINT FAIL (0x");
        putx(gintsts as u64);
        puts(")\r\n");
        fail += 1;
    }
    if haint & 1 != 0 {
        puts("eth: HAINT[0] OK\r\n");
        pass += 1;
    } else {
        puts("eth: HAINT FAIL\r\n");
        fail += 1;
    }
    if pend1 & IRQ_USB != 0 {
        puts("eth: PENDING1 bit9 (USB IRQ) OK\r\n");
        pass += 1;
    } else {
        puts("eth: PENDING1 FAIL (0x");
        putx(pend1 as u64);
        puts(")\r\n");
        fail += 1;
    }
    // W1C the channel + GINTSTS so the line drops (no storm).
    mmio_write(HAINTMSK, 0);
    mmio_write(GINTMSK, 0);
    mmio_write(GINTSTS, GINTSTS_HCHINT);

    // --- RX: loopback read via EP1 IN (model echoes the TX frame) ---
    mmio_write(HCDMA0, rx_pa);
    mmio_write(HCTSIZ0, 1536);
    mmio_write(HCINTMSK0, HCINT_XFERCOMPL | HCINT_CHHLTD);
    mmio_write(HCCHAR0, hcchar(1, true));
    let hcint = wait_hcint();
    if hcint & HCINT_XFERCOMPL != 0 {
        puts("eth: RX XFERCOMPL OK\r\n");
        pass += 1;
    } else {
        puts("eth: RX FAIL (0x");
        putx(hcint as u64);
        puts(")\r\n");
        fail += 1;
    }
    // RX layout: 4-byte header (length) + frame. Verify the echoed
    // payload matches what we sent.
    let ok = unsafe {
        let n = u32::from_le_bytes([RX_BUF[0], RX_BUF[1], RX_BUF[2], RX_BUF[3]]) as usize;
        n == framelen + 4
            && RX_BUF[4..10] == [0xff; 6]
            && RX_BUF[18..18 + payload.len()] == *payload
    };
    if ok {
        puts("eth: loopback frame OK\r\n");
        pass += 1;
    } else {
        puts("eth: loopback FAIL\r\n");
        fail += 1;
    }

    puts("eth: ");
    putx(pass as u64);
    puts(" pass\r\n");
    if fail == 0 {
        puts("eth: ALL PASS\r\n");
    } else {
        puts("eth: FAIL\r\n");
    }
    mmio_write(USB_DONE, 1);
    loop {
        core::hint::spin_loop();
    }
}
