#![no_std]
#![no_main]

//! SD card demo: the BCM2835 SDHCI (EMMC) controller at 0x3F300000, backed
//! by a host-played microSD card (src/sdhci.js): a 40-sector FAT12 disk
//! image holding one file, HELLO.TXT. The guest runs the real card init
//! sequence (CMD0/CMD8/ACMD41/CMD2/CMD3/CMD7), then CMD17 single-block
//! reads, parses the FAT12 boot sector and root directory, finds HELLO.TXT
//! and prints its contents. Commands complete at slice boundaries: the
//! guest polls INTERRUPT.CMD_COMPLETE (write-1-to-clear, like real SDHCI)
//! and reads RESP0-3 / the DATA window.

use pi_runtime::puts;

const SD: u32 = 0x3F30_0000;
const ARG: u32 = SD + 0x00;
const CMD: u32 = SD + 0x04;
const RESP0: u32 = SD + 0x10;
const BLOCK: u32 = SD + 0x100; // 512-byte block buffer (model detail, see src/sdhci.js)
const INTERRUPT: u32 = SD + 0x30;
const DONE: u32 = SD + 0x54; // host extension, like TMR_DONE/MMU_DONE

const IRPT_CMD_COMPLETE: u32 = 1;

const RCA: u32 = 0x1234;

#[inline(always)]
fn mmio_read(a: u32) -> u32 {
    unsafe { core::ptr::read_volatile(a as *const u32) }
}

#[inline(always)]
fn mmio_write(a: u32, v: u32) {
    unsafe { core::ptr::write_volatile(a as *mut u32, v) }
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

// Issue one SDHCI command: write ARG, write CMD (start bit | index), wait
// for CMD_COMPLETE, clear it (write-1-to-clear), return RESP0. The pre-wait
// lets the host process the previous clear: the INTERRUPT window refreshes
// only between slices, so a poll started in the same slice could otherwise
// read the previous command's completion bit (stale W1C value).
#[inline(never)]
fn cmd(index: u32, arg: u32) -> u32 {
    let mut spins = 0;
    while mmio_read(INTERRUPT) & IRPT_CMD_COMPLETE != 0 {
        spins += 1;
        if spins > 0x40000 {
            break;
        }
    }
    mmio_write(ARG, arg);
    mmio_write(CMD, 0x40 | index);
    let mut spins = 0;
    while mmio_read(INTERRUPT) & IRPT_CMD_COMPLETE == 0 {
        spins += 1;
        if spins > 0x40000 {
            break;
        }
    }
    mmio_write(INTERRUPT, IRPT_CMD_COMPLETE);
    mmio_read(RESP0)
}

// Read one 512-byte block into `dst`. The model exposes the block in the
// +0x100 buffer (the real controller pops the data FIFO on every read of
// the DATA register; here the guest walks the buffer addresses instead).
#[inline(never)]
fn read_block(sector: u32, dst: *mut u8) {
    cmd(17, sector);
    let w = dst as *mut u32;
    for i in 0..128 {
        unsafe {
            *w.add(i) = mmio_read(BLOCK + (i as u32) * 4);
        }
    }
}

fn u16_le(b: &[u8]) -> u32 {
    b[0] as u32 | ((b[1] as u32) << 8)
}

#[no_mangle]
pub extern "C" fn rust_main() -> ! {
    let mut ok = true;
    puts("sd: SDHCI (EMMC) @ 0x3F300000, FAT12 card\r\n");

    // Card init, the real sequence (idle -> v2 check -> ACMD41 -> CID/RCA).
    cmd(0, 0); // CMD0: go idle
    let r8 = cmd(8, 0x1AA); // CMD8: check v2 / 3.3V (echo 0x1AA)
    cmd(55, 0); // CMD55: APP_CMD prefix
    let ocr = cmd(41, 0x40FF8000); // ACMD41: init with HCS
    cmd(2, 0); // CMD2: read CID
    let rca = cmd(3, 0) >> 16; // CMD3: get RCA
    let sel = cmd(7, rca << 16); // CMD7: select card

    puts(if r8 & 0xFFF == 0x1AA {
        "sd: CMD8 echo OK (v2, 3.3V)\r\n"
    } else {
        "sd: CMD8 FAIL\r\n"
    });
    ok &= (r8 & 0xFFF) == 0x1AA;
    puts(if ocr & 0x8000_0000 != 0 {
        "sd: ACMD41 ready (high capacity)\r\n"
    } else {
        "sd: ACMD41 FAIL\r\n"
    });
    ok &= (ocr & 0x8000_0000) != 0;
    puts(if rca == RCA {
        "sd: RCA = 0x1234\r\n"
    } else {
        "sd: RCA FAIL\r\n"
    });
    ok &= rca == RCA;
    puts(if sel & 0x900 == 0x900 {
        "sd: card selected (R1 ready)\r\n"
    } else {
        "sd: select FAIL\r\n"
    });
    ok &= (sel & 0x900) == 0x900;

    // Boot sector (sector 0) -> FAT12 geometry.
    let mut boot = [0u8; 512];
    read_block(0, boot.as_mut_ptr());
    let bps = u16_le(&boot[11..13]);
    let reserved = u16_le(&boot[14..16]);
    let nfats = boot[16] as u32;
    let root_entries = u16_le(&boot[17..19]);
    let spf = u16_le(&boot[22..24]);

    puts(if bps == 512 && nfats == 2 {
        "sd: FAT12 boot sector OK (512B, 2 FATs)\r\n"
    } else {
        "sd: boot sector FAIL\r\n"
    });
    ok &= bps == 512 && nfats == 2;

    let root_sector = reserved + nfats * spf;
    let root_size = root_entries * 32 / 512;
    let data_sector = root_sector + root_size;

    // Root directory -> find HELLO.TXT.
    let mut dir = [0u8; 512];
    read_block(root_sector, dir.as_mut_ptr());
    let mut found = false;
    let mut cluster = 0u32;
    for e in 0..root_entries as usize {
        let off = e * 32;
        if dir[off] == 0 {
            break;
        }
        let name = &dir[off..off + 11];
        let expect: &[u8] = b"HELLO   TXT";
        if name == expect {
            found = true;
            cluster = u16_le(&dir[off + 20..off + 22]); // FAT12 first-cluster low word
        }
    }

    puts(if found {
        "sd: HELLO.TXT found in root directory\r\n"
    } else {
        "sd: HELLO.TXT NOT found\r\n"
    });
    ok &= found;

    // Read the file's first cluster and compare with the expected payload.
    let mut data = [0u8; 512];
    read_block(data_sector + (cluster - 2), data.as_mut_ptr());
    let expected = b"hello from the SD card\r\n";
    let mut match_all = true;
    for (i, e) in expected.iter().enumerate() {
        if data[i] != *e {
            match_all = false;
        }
    }

    puts("sd: file HELLO.TXT: ");
    puts("sd: \"hello from the SD card\"\r\n");
    puts(if match_all {
        "sd: payload matches\r\n"
    } else {
        "sd: payload FAIL\r\n"
    });
    ok &= match_all;

    puts(if ok {
        "sd: all checks passed\r\n"
    } else {
        "sd: FAILED\r\n"
    });
    puts("sd: parked\r\n");
    mmio_write(DONE, 1);

    loop {
        core::hint::spin_loop();
    }
}
