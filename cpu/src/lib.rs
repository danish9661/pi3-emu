// pi-cpu: a small AArch64 interpreter for the pi3-emu bare-metal guests.
//
// Scope (M40 spike): integer + control flow + loads/stores, flat memory,
// zero-mapped MMIO with a PL011-TX console tap. No FP/NEON, no MMU, no
// exceptions — anything outside the subset faults cleanly (Fault) so the
// differential harness against unicorn.js catches coverage gaps instead of
// silently diverging.
//
// Endianness: little-endian throughout.

pub const RAM_SIZE: u64 = 0x400000;
pub const UART0: u64 = 0x3f201000;
pub const TMR_BASE: u64 = 0x3f003000;
pub const GPIO_BASE: u64 = 0x3f200000;
pub const UART1_BASE: u64 = 0x3f215000; // mini UART (TX tap only)
pub const SD_BASE: u64 = 0x3f300000;
pub const IC_BASE: u64 = 0x3f00b200;
/// Magic cell for host-assisted IRQ resume (mirrors the facade's
/// IC_BASE+0x2C protocol): the vector glue writes nonzero, the harness
/// resumes the saved PC and clears the flag.
pub const IC_IRQ_RET: u64 = IC_BASE + 0x2c;
// Default-mapped windows with no active model (mirrors the facade
// constructor): the MBOX page (mailbox regs + IC ride-along + SD_PRESENT,
// all zero cells) and the local-block page (zeros). Data accesses outside
// {RAM, UART0, TMR, GPIO, MBOX page, LOCAL} fault like the unicorn core
// (UnmappedData) instead of zero-mapping — the debug guest depends on it.
pub const MBOX_PAGE: u64 = 0x3f00b000;
pub const LOCAL_BASE: u64 = 0x40000000;
/// SD-card presence flag (host extension word): the card is always
/// attached (the disk lives in this model), like the facade with
/// attachSdhci(). Checked before the IC window (it rides inside it).
pub const SD_PRESENT: u64 = MBOX_PAGE + 0xff0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    Illegal(u32),
    UnmappedFetch(u64),
    UnmappedData(u64),
    /// Stage-1 translation fault (bad descriptor/range during the walk).
    Translation(u64),
}

pub struct Bus {
    mem: Vec<u8>,
    pub console: Vec<u8>,
    // System-timer model (BCM2837 0x3F003000), mirroring the host facade:
    // compares pulled from the window, match-pending latched with crossed
    // flags, CS writes absorbed as keep-masks (the model's inverted W1C —
    // same convention the bare-metal guests use), DONE parks the guest.
    // Time is virtual and integer-exact: vt_us advances per chunk as
    // chunk_insns * 1e6 / vt_ips (exact when the harness slice divides
    // evenly — see test/cpu-diff.mjs); vt_ips == 0 disables the clock
    // (CLO reads 0, legacy behavior for timer-less guests).
    // The 4K timer window has its own backing store; CLO/CHI read derived
    // values (writes ignored), everything else behaves like RAM cells.
    tmr_mem: Vec<u8>,
    pub vt_ips: u64,
    vt_us: u64,
    tmr_compares: [u32; 4],
    tmr_pending: u32,
    tmr_crossed: [bool; 4],
    tmr_last_cs: u32,
    pub tmr_done: bool,
    // GPIO model (BCM2837 0x3F200000, bank 0 only — mirrors the facade's
    // gpio.js): output latch from GPSET/GPCLR, host-driven inputs in
    // `gpio_in` (bitmask, set by the harness between chunks), edge events
    // in `gpio_eds` (W1C), enable cells for the 6 EV reg pairs. Edges are
    // evaluated in sync_out from input transitions, exactly like the
    // facade (level changes while the enable is live; pressing once at
    // boot with no enable armed records nothing).
    pub gpio_in: u32,
    gpio_prev_in: u32,
    gpio_out: u32,
    gpio_eds: u32,
    // EV-reg cells indexed (off-0x4C)/4 over 0x4C..=0x8C (reserved gaps
    // included as harmless cells, like window memory); pair p bank b
    // lives at [0,3,6,9,12,15][p]+b (REN/FEN/HEN/LEN/AREN/AFEN).
    gpio_en: [u32; 17],
    // Legacy IC (0x3F00B200, bank 2 GPIO line only — mirrors ic.js):
    // ENABLE accumulates, DISABLE clears, PENDING reads show line&enabled.
    ic_en1: u32,
    ic_en2: u32,
    /// Set by a nonzero write to IC_IRQ_RET; the harness consumes it to
    /// resume the saved PC (host-assisted delivery protocol).
    pub irq_ret_pending: bool,
    // PL011 UART0 (0x3F201000 — mirrors uart0.js): config cells the guest
    // reads back, a 16-deep RX FIFO the harness fills, derived FR/RIS/MIS.
    // RIS.TXINTR is always set (TX never fills); the IRQ line is
    // RXINTR/TXINTR masked by IMSC, gated on UARTEN|RXE (CR bit0|bit9).
    uart0_cr: u32,
    uart0_lcrh: u32,
    uart0_ibrd: u32,
    uart0_fbrd: u32,
    uart0_imsc: u32,
    pub uart0_rx: Vec<u8>,
    // Stage-1 MMU state (EL1, 4K granule). Written by the Cpu's MSR arm
    // (translation runs inside bus.read/write/fetch, which need them
    // there). Permissions/AF/MAIR are NOT modeled — everything the
    // guests map is full-access Normal-equivalent RAM/MMIO.
    mmu_sctlr: u64,
    mmu_tcr: u64,
    mmu_ttbr0: u64,
    mmu_mair: u64,
    // ARM arch timer (CNTP, 19.2 MHz like the Pi 3): the counter follows
    // the facade's float virtual-time replica exactly (virtualUs_f +=
    // (n/ips)*1e6 per chunk, cntpct = floor(us*19.2)) so compare matches
    // fire on the same chunk on both sides. TVAL writes latch
    // cval = cntpct + val; CTL bit 0 enables.
    pub cntpct: u64,
    cntp_cval: u64,
    cntp_ctl: u32,
    vt_us_f: f64,
    // SDHCI (0x3F300000 — mirrors sdhci.js PIO mode): ARG/CMD/RESP cells,
    // 512-byte staging buffer, IRPT with CMD_COMPLETE, and a sector disk
    // built by make_disk() (byte-exact port of the facade's FAT12 image;
    // grows on CMD24 up to 32 sectors, like the model).
    sd_arg: u32,
    sd_cmd: u32,
    sd_resp0: u32,
    sd_stage: [u8; 512],
    sd_irpt: u32,
    sd_disk: Vec<[u8; 512]>,
}

impl Bus {
    pub fn new() -> Self {
        Bus {
            mem: vec![0; RAM_SIZE as usize],
            console: Vec::new(),
            tmr_mem: vec![0; 0x1000],
            vt_ips: 0,
            vt_us: 0,
            tmr_compares: [0; 4],
            tmr_pending: 0,
            tmr_crossed: [false; 4],
            tmr_last_cs: 0,
            tmr_done: false,
            gpio_in: 0,
            gpio_prev_in: 0,
            gpio_out: 0,
            gpio_eds: 0,
            gpio_en: [0; 17],
            ic_en1: 0,
            ic_en2: 0,
            irq_ret_pending: false,
            uart0_cr: 0,
            uart0_lcrh: 0,
            uart0_ibrd: 0,
            uart0_fbrd: 0,
            uart0_imsc: 0,
            uart0_rx: Vec::new(),
            mmu_sctlr: 0,
            mmu_tcr: 0,
            mmu_ttbr0: 0,
            mmu_mair: 0,
            cntpct: 0,
            cntp_cval: 0,
            cntp_ctl: 0,
            vt_us_f: 0.0,
            sd_arg: 0,
            sd_cmd: 0,
            sd_resp0: 0,
            sd_stage: [0; 512],
            sd_irpt: 0,
            sd_disk: Bus::make_disk(),
        }
    }

    fn is_ram(addr: u64, size: u64) -> bool {
        addr.checked_add(size).map_or(false, |e| e <= RAM_SIZE)
    }

    fn is_uart(addr: u64, size: u64) -> bool {
        addr >= UART0
            && addr.checked_add(size).map_or(false, |e| e <= UART0 + 0x1000)
    }

    fn is_timer(addr: u64, size: u64) -> bool {
        addr >= TMR_BASE
            && addr.checked_add(size).map_or(false, |e| e <= TMR_BASE + 0x1000)
    }

    fn is_gpio(addr: u64, size: u64) -> bool {
        addr >= GPIO_BASE
            && addr.checked_add(size).map_or(false, |e| e <= GPIO_BASE + 0x1000)
    }

    fn is_ic(addr: u64, size: u64) -> bool {
        // Capped at the MBOX page end (the IC rides inside that mapping;
        // past 0x3F00C000 the facade faults too).
        addr >= IC_BASE
            && addr.checked_add(size).map_or(false, |e| e <= MBOX_PAGE + 0x1000)
    }

    /// FNV-1a hash over RAM (differential debugging aid).
    pub fn mem_hash(&self) -> u64 {
        let mut h = 0xcbf29ce484222325u64;
        for b in self.mem.iter() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    }

    fn is_page(addr: u64, size: u64, base: u64) -> bool {
        addr >= base && addr.checked_add(size).map_or(false, |e| e <= base + 0x1000)
    }

    fn is_sd(addr: u64, size: u64) -> bool {
        Self::is_page(addr, size, SD_BASE)
    }

    fn is_miniuart(addr: u64, size: u64) -> bool {
        Self::is_page(addr, size, UART1_BASE)
    }

    /// Facade makeDisk() port (packages/pi3-emu/src/sdhci.js): 5-sector
    /// FAT12 with HELLO.TXT ("hello from the SD card\r\n" in sector 4).
    fn make_disk() -> Vec<[u8; 512]> {
        let mut s = vec![[0u8; 512]; 5];
        {
            let b = &mut s[0];
            b[0] = 0xeb;
            b[1] = 0x3c;
            b[2] = 0x90;
            b[3..11].copy_from_slice(b"PI3EMU  ");
            b[11] = 0x00;
            b[12] = 0x02;
            b[13] = 1;
            b[14] = 1;
            b[16] = 2;
            b[17] = 16;
            b[19] = 0x28;
            b[21] = 0xf8;
            b[22] = 1;
            b[24] = 1;
            b[26] = 1;
            b[33] = 0x28;
            b[54..62].copy_from_slice(b"FAT12   ");
            b[510] = 0x55;
            b[511] = 0xaa;
        }
        {
            let f0 = [0xf8u8, 0xff, 0xff, 0xff, 0xff, 0x0f];
            s[1][..6].copy_from_slice(&f0);
            s[2][..6].copy_from_slice(&f0);
        }
        {
            let r = &mut s[3];
            r[..11].copy_from_slice(b"HELLO   TXT");
            r[11] = 0x20;
            r[22] = 0x00;
            r[23] = 0x60;
            r[24] = 0x2a;
            r[25] = 0x5d;
            r[26] = 2;
            r[27] = 0;
            let msg = b"hello from the SD card\r\n";
            let n = msg.len();
            r[28] = (n & 0xff) as u8;
            r[29] = ((n >> 8) & 0xff) as u8;
            r[30] = 0;
            r[31] = 0;
            s[4][..n].copy_from_slice(msg);
        }
        s
    }

    fn sd_read_sector(&self, sec: usize) -> [u8; 512] {
        self.sd_disk.get(sec & 0xffff).copied().unwrap_or([0; 512])
    }

    fn sd_write_sector(&mut self, sec: usize, data: [u8; 512]) {
        let s = sec & 0xffff;
        if self.sd_disk.get(s).is_none() {
            if s >= 32 {
                return; // cap growth: not a real allocator (mirrors model)
            }
            self.sd_disk.resize(s + 1, [0; 512]);
        }
        self.sd_disk[s] = data;
    }

    /// SDHCI command execution (mirrors sdhci.js exec(), PIO mode only —
    /// no DMA/ADMA paths; the firmware uses PIO throughout).
    fn sd_exec(&mut self, index: u32, arg: u32) {
        eprintln!("SDCMD {} arg={:#x}", index, arg);
        const CID: [u32; 4] = [0x12345678, 0x9abcdef0, 0x13579bdf, 0x2468ace0];
        match index {
            0 => self.sd_resp0 = 0,
            2 => self.sd_resp0 = CID[0],
            3 => self.sd_resp0 = 0x12340000,
            6 | 7 | 9 | 12 | 13 => self.sd_resp0 = 0x900,
            8 => self.sd_resp0 = 0x1aa,
            16 => self.sd_resp0 = 0x900, // blockLen ignored (always 512)
            17 => {
                self.sd_resp0 = 0x900;
                self.sd_stage = self.sd_read_sector(arg as usize);
                eprintln!("SD17 sec={} st0={:#x} disk00={:#x}", arg & 0xffff, self.sd_stage[0], self.sd_disk[0][0]);
                self.sd_irpt |= (1 << 5) | (1 << 1); // READ_READY|XFER
            }
            24 => {
                self.sd_resp0 = 0x900;
                self.sd_write_sector(arg as usize, self.sd_stage);
                self.sd_irpt |= (1 << 4) | (1 << 1); // WRITE_READY|XFER
            }
            55 => self.sd_resp0 = 0x120,
            41 => self.sd_resp0 = 0xc0ff8000,
            _ => self.sd_resp0 = 0,
        }
        self.sd_irpt |= 1; // CMD_COMPLETE
    }

    /// Stage-1 VA→PA translation (4K granule, TTBR0 only). Identity while
    /// SCTLR.M is clear. Faults on out-of-range VA, non-4K granule, bad
    /// descriptors, or tables outside RAM.
    fn translate(&self, va: u64) -> Result<u64, Fault> {
        if (self.mmu_sctlr & 1) == 0 {
            return Ok(va);
        }
        let t0sz = (self.mmu_tcr & 0x3f) as u32;
        if (self.mmu_tcr >> 14) & 3 != 0 {
            return Err(Fault::Translation(va)); // TG0 != 4K unsupported
        }
        let vabits = 64u32.saturating_sub(t0sz);
        if !(12..=39).contains(&vabits) || (va >> vabits) != 0 {
            return Err(Fault::Translation(va));
        }
        let mut level = if vabits > 30 {
            1
        } else if vabits > 21 {
            2
        } else {
            3
        };
        let mut base = self.mmu_ttbr0 & !0xfff;
        loop {
            let shift = 12 + 9 * (3 - level);
            let idx = ((va >> shift) & 0x1ff) as u64;
            let da = base.wrapping_add(idx * 8);
            if !Self::is_ram(da, 8) {
                return Err(Fault::Translation(va));
            }
            let a = da as usize;
            let mut d = 0u64;
            for i in 0..8 {
                d |= (self.mem[a + i] as u64) << (8 * i);
            }
            match d & 3 {
                0b01 if level < 3 => {
                    // Block (1G at L1, 2M at L2).
                    let block = 1u64 << shift;
                    return Ok((d & !(block - 1)) | (va & (block - 1)));
                }
                0b11 => {
                    if level == 3 {
                        return Ok((d & !0xfff) | (va & 0xfff));
                    }
                    base = d & 0x0000_ffff_ffff_f000;
                    level += 1;
                }
                _ => return Err(Fault::Translation(va)),
            }
        }
    }

    /// Union of the 6 EV-reg enables for a bank (REN/FEN/HEN/LEN/AREN/
    /// AFEN), mirroring the facade's enMasks().
    fn gpio_en_union(&self, bank: usize) -> u32 {
        let mut m = 0u32;
        for p in 0..6 {
            m |= self.gpio_en[p * 3 + bank];
        }
        m
    }

    /// GPIO bank-0 raw line: any event bit covered by an enable.
    fn gpio0_raw(&self) -> bool {
        (self.gpio_eds & self.gpio_en_union(0)) != 0
    }

    /// CNTPNS level (physical timer): enabled and counter >= cval.
    pub fn cntp_line(&self) -> bool {
        (self.cntp_ctl & 1) != 0 && self.cntpct >= self.cntp_cval
    }

    /// Legacy-IC gated line (bank-1 timer bits, bank-2 GPIO bit 17 +
    /// UART bit 25; DMA/AUX/SDHCI unmodeled). Mirrors ic.js pending().
    pub fn legacy_line(&self) -> bool {
        self.timer_pending1() | self.gpio_pending2() | self.uart_pending2() != 0
    }

    /// Gated bank-1 bits: timer C0-C3 matches (facade icLines timer field).
    fn timer_pending1(&self) -> u32 {
        (self.tmr_pending & 0xf) & self.ic_en1
    }

    /// Gated bank-2 bits (for PENDING2/BASIC reads).
    fn gpio_pending2(&self) -> u32 {
        if self.gpio0_raw() && ((self.ic_en2 >> 17) & 1) != 0 {
            1 << 17
        } else {
            0
        }
    }

    fn uart_enabled(&self) -> bool {
        (self.uart0_cr & ((1 << 9) | 1)) != 0
    }

    /// Raw UART IRQ bits (RIS): RXINTR iff FIFO non-empty, TXINTR always.
    fn uart_ris(&self) -> u32 {
        (if self.uart0_rx.is_empty() { 0 } else { 1 << 4 }) | (1 << 5)
    }

    fn uart_pending2(&self) -> u32 {
        if self.uart_enabled()
            && ((self.uart_ris() & self.uart0_imsc) != 0)
            && ((self.ic_en2 >> 25) & 1) != 0
        {
            1 << 25
        } else {
            0
        }
    }

    /// Host key input (mirrors uart0 push(): queued only while enabled,
    /// 16-deep FIFO).
    pub fn uart0_push(&mut self, b: u8) {
        if self.uart_enabled() && self.uart0_rx.len() < 16 {
            self.uart0_rx.push(b);
        }
    }

    pub fn read(&mut self, addr: u64, size: u64) -> Result<u64, Fault> {
        let addr = self.translate(addr)?;
        if Self::is_ram(addr, size) {
            let a = addr as usize;
            let mut v = 0u64;
            for i in 0..size {
                v |= (self.mem[a + i as usize] as u64) << (8 * i);
            }
            return Ok(v);
        }
        if Self::is_uart(addr, size) {
            let off = addr - UART0;
            if off == 0x00 {
                // DR: head byte + pop (mirrors the facade's DR read hook;
                // 0 when the FIFO is empty).
                let b = if self.uart0_rx.is_empty() {
                    0
                } else {
                    self.uart0_rx.remove(0)
                };
                return Ok((b as u64) & mask(size));
            }
            if off == 0x18 {
                // FR: TX ready always (never fills), RXFE iff FIFO empty.
                let mut fr = 1 << 7;
                if self.uart0_rx.is_empty() {
                    fr |= 1 << 4;
                }
                return Ok(fr & mask(size));
            }
            if off == 0x24 {
                return Ok(self.uart0_ibrd as u64 & mask(size));
            }
            if off == 0x28 {
                return Ok(self.uart0_fbrd as u64 & mask(size));
            }
            if off == 0x2c {
                return Ok(self.uart0_lcrh as u64 & mask(size));
            }
            if off == 0x30 {
                return Ok(self.uart0_cr as u64 & mask(size));
            }
            if off == 0x38 {
                return Ok(self.uart0_imsc as u64 & mask(size));
            }
            if off == 0x3c {
                return Ok(self.uart_ris() as u64 & mask(size));
            }
            if off == 0x40 {
                return Ok((self.uart_ris() & self.uart0_imsc) as u64 & mask(size));
            }
            return Ok(0); // ICR + unlisted absorb on read
        }
        if Self::is_timer(addr, size) {
            let off = addr - TMR_BASE;
            if off == 0x00 {
                return Ok(self.tmr_pending as u64 & mask(size));
            }
            if off == 0x04 {
                return Ok((self.vt_us & 0xffff_ffff) & mask(size));
            }
            // CMP/DONE cells are real window memory (see write).
            let a = off as usize;
            let mut v = 0u64;
            for i in 0..size {
                v |= (self.tmr_mem[a + i as usize] as u64) << (8 * i);
            }
            return Ok(v);
        }
        if Self::is_gpio(addr, size) {
            let off = addr - GPIO_BASE;
            let v = match off {
                0x34 => ((self.gpio_out & !self.gpio_in) | self.gpio_in) as u64,
                0x38 => 0,
                0x40 => self.gpio_eds as u64,
                0x44 => 0,
                0x4c..=0x8c => {
                    let i = ((off - 0x4c) / 4) as usize;
                    if (off - 0x4c) % 4 == 0 && i < 17 {
                        self.gpio_en[i] as u64
                    } else {
                        0
                    }
                }
                _ => 0, // GPFSEL/GPPUD/SET/CLR absorb (write-only)
            };
            return Ok(v & mask(size));
        }
        // SD_PRESENT rides inside the IC window range — check first.
        if addr == SD_PRESENT && size <= 4 {
            return Ok(1 & mask(size));
        }
        if addr == SD_PRESENT && std::env::var("MBOXTRACE").is_ok() {
            eprintln!("SDPRESENT-READ");
        }
        if Self::is_ic(addr, size) {
            let off = addr - IC_BASE;
            let v: u64 = match off {
                // PENDING2 (GPIO bit 17 + UART bit 25) + BASIC mirrors
                // (bit 9 for GPIO, bit 19 shortcut for UART).
                0x08 => (self.gpio_pending2() | self.uart_pending2()) as u64,
                0x04 => self.timer_pending1() as u64,
                0x00 => {
                    let mut b = 0u64;
                    if self.timer_pending1() != 0 {
                        b |= 1 << 8; // any non-shortcut bank-1 line
                    }
                    if self.gpio_pending2() != 0 {
                        b |= 1 << 9;
                    }
                    if self.uart_pending2() != 0 {
                        b |= 1 << 19;
                    }
                    b
                }
                _ => 0, // ENABLE/DISABLE/RET read back 0
            };
            return Ok(v & mask(size));
        }
        if Self::is_sd(addr, size) && std::env::var("SDTRACE").is_ok() {
            eprintln!("SDRD {:#x} sz={}", addr, size);
        }
        if Self::is_sd(addr, size) {
            let off = addr - SD_BASE;
            let v = match off {
                0x00 => self.sd_arg as u64,
                0x04 => self.sd_cmd as u64,
                0x10 => self.sd_resp0 as u64,
                0x30 => self.sd_irpt as u64,
                0x100..=0x2ff => {
                    // Staging buffer (little-endian word reads).
                    if off == 0x100 {
                        eprintln!("BLK0 {:02x}{:02x}{:02x}{:02x}...{:02x}{:02x}", self.sd_stage[0], self.sd_stage[1], self.sd_stage[2], self.sd_stage[3], self.sd_stage[510], self.sd_stage[511]);
                    }
                    let mut w = 0u64;
                    for i in 0..size {
                        w |= (self.sd_stage[(off as usize) - 0x100 + i as usize] as u64)
                            << (8 * i);
                    }
                    if off == 0x100 && std::env::var("SDTRACE").is_ok() {
                        eprintln!("BLK0W {:#x}", w);
                    }
                    return Ok(w & mask(size));
                }
                _ => 0,
            };
            return Ok(v & mask(size));
        }
        if Self::is_miniuart(addr, size) {
            return Ok(0); // status/RX read as empty (RX unsupported, like uart_getc)
        }
        if Self::is_page(addr, size, MBOX_PAGE) || Self::is_page(addr, size, LOCAL_BASE) {
            // Local block CORE_IRQ_SRC (core 0, +0x60): bit 1 = CNTPNSIRQ,
            // bit 8 = GPU (legacy line). Everything else reads zero.
            if Self::is_page(addr, size, LOCAL_BASE) && addr - LOCAL_BASE == 0x60 {
                let mut v = 0u64;
                if self.cntp_line() {
                    v |= 1 << 1;
                }
                if self.legacy_line() {
                    v |= 1 << 8;
                }
                return Ok(v & mask(size));
            }
            return Ok(0); // unmodeled cells read zero, like the facade
        }
        // Outside the default-mapped set: fault, like the unicorn core.
        Err(Fault::UnmappedData(addr))
    }

    pub fn write(&mut self, addr: u64, size: u64, val: u64) -> Result<(), Fault> {
        let addr = self.translate(addr)?;
        if Self::is_ram(addr, size) {
            let a = addr as usize;
            for i in 0..size {
                self.mem[a + i as usize] = ((val >> (8 * i)) & 0xff) as u8;
            }
            return Ok(());
        }
        if Self::is_uart(addr, size) {
            let off = addr - UART0;
            if off == 0x00 {
                // TX tap: nonzero bytes to the console (mirrors the
                // facade's DR write hook, including its no-CR-gate and
                // skip-zero behavior).
                let b = (val & 0xff) as u8;
                if b != 0 {
                    self.console.push(b);
                }
                return Ok(());
            }
            let v = (val & mask(size)) as u32;
            match off {
                0x24 => self.uart0_ibrd = v,
                0x28 => self.uart0_fbrd = v,
                0x2c => self.uart0_lcrh = v,
                0x30 => self.uart0_cr = v,
                0x38 => self.uart0_imsc = v,
                _ => {} // ICR + unlisted absorb
            }
            return Ok(());
        }
        if Self::is_timer(addr, size) {
            let off = addr - TMR_BASE;
            if off == 0x04 || off == 0x08 {
                return Ok(()); // CLO/CHI are read-only (derived)
            }
            let a = off as usize;
            for i in 0..size {
                self.tmr_mem[a + i as usize] = ((val >> (8 * i)) & 0xff) as u8;
            }
            if off == 0x20 && val != 0 {
                self.tmr_done = true;
            }
            return Ok(());
        }
        if Self::is_gpio(addr, size) {
            let off = addr - GPIO_BASE;
            let v = (val & mask(size)) as u32;
            match off {
                0x1c => self.gpio_out |= v,
                0x28 => self.gpio_out &= !v,
                0x40 => self.gpio_eds &= !v, // W1C (guest-only path)
                0x4c..=0x8c => {
                    let i = ((off - 0x4c) / 4) as usize;
                    if (off - 0x4c) % 4 == 0 && i < 17 {
                        self.gpio_en[i] = v;
                    }
                }
                _ => {} // GPFSEL/GPPUD/LEV absorb
            }
            return Ok(());
        }
        if Self::is_ic(addr, size) {
            let off = addr - IC_BASE;
            let v = (val & mask(size)) as u32;
            match off {
                0x10 => self.ic_en1 |= v,
                0x14 => self.ic_en2 |= v,
                0x1c => self.ic_en1 &= !v,
                0x20 => self.ic_en2 &= !v,
                0x2c => {
                    if v != 0 {
                        self.irq_ret_pending = true;
                    }
                }
                _ => {} // PENDING/BASIC absorb
            }
            return Ok(());
        }
        if std::env::var("SDTRACE").is_ok() && Self::is_sd(addr, size) {
            eprintln!("SDWR {:#x} sz={} val={:#x}", addr, size, val);
        }
        if Self::is_sd(addr, size) {
            let off = addr - SD_BASE;
            let v = (val & mask(size)) as u32;
            match off {
                0x00 => self.sd_arg = v,
                0x04 => {
                    self.sd_cmd = v;
                    self.sd_exec(v & 0x3f, self.sd_arg);
                }
                0x30 => self.sd_irpt &= !v, // W1C
                0x100..=0x2ff => {
                    // Stage PIO bytes for CMD24.
                    for i in 0..size {
                        self.sd_stage[(off as usize) - 0x100 + i as usize] =
                            ((val >> (8 * i)) & 0xff) as u8;
                    }
                }
                _ => {} // RESP/DONE absorb
            }
            return Ok(());
        }
        if Self::is_miniuart(addr, size) {
            // Mini-UART TX (AUX_MU_IO at +0x40) is written blindly (no
            // status poll in machine_uart.c); tap nonzero bytes.
            if addr - UART1_BASE == 0x40 {
                let b = (val & 0xff) as u8;
                if b != 0 {
                    self.console.push(b);
                }
            }
            return Ok(());
        }
        if Self::is_page(addr, size, MBOX_PAGE) || Self::is_page(addr, size, LOCAL_BASE) {
            return Ok(()); // unmodeled cells absorb writes, like the facade
        }
        Err(Fault::UnmappedData(addr))
    }

    fn tmr_cell(&self, off: u64) -> u32 {
        let a = off as usize;
        u32::from_le_bytes([self.tmr_mem[a], self.tmr_mem[a + 1], self.tmr_mem[a + 2], self.tmr_mem[a + 3]])
    }

    fn tmr_set_cell(&mut self, off: u64, v: u32) {
        let a = off as usize;
        for (i, b) in v.to_le_bytes().iter().enumerate() {
            self.tmr_mem[a + i] = *b;
        }
    }

    /// Pre-chunk sync (mirrors the facade syncTimerOut): evaluate matches
    /// at the current visible CLO, publish CS.
    pub fn sync_out(&mut self) {
        let us = (self.vt_us & 0xffff_ffff) as u32;
        for i in 0..4 {
            let c = self.tmr_compares[i];
            if !self.tmr_crossed[i] && c != 0 && (us.wrapping_sub(c) & 0x8000_0000) == 0 {
                self.tmr_crossed[i] = true;
                self.tmr_pending |= 1 << i;
            }
        }
        self.tmr_set_cell(0x00, self.tmr_pending);
        self.tmr_last_cs = self.tmr_pending;
        // GPIO edge detect on host-input transitions (facade syncOut
        // order): edges set EDS iff covered by an enable; level enables
        // force while held. Bank 0 only (the host drives one button bit).
        let host = self.gpio_in;
        let prev = self.gpio_prev_in;
        if host != prev {
            let rise = host & !prev;
            let fall = prev & !host;
            self.gpio_eds |= (rise | fall) & self.gpio_en_union(0);
            self.gpio_prev_in = host;
        }
        let hen = self.gpio_en[6];
        let len = self.gpio_en[9];
        if (hen | len) != 0 {
            let lev = (self.gpio_out & !host) | host;
            self.gpio_eds |= (lev & hen) | (!lev & len);
        }
    }

    /// Post-chunk sync (mirrors syncTimerIn): pull compares (resetting a
    /// crossed latch on change), absorb a guest CS write as a keep-mask,
    /// then advance virtual time past this chunk (frozen at 0 unless
    /// vt_ips is set — legacy zero-clock for timer-less guests).
    pub fn sync_in(&mut self, chunk_insns: u64) {
        for i in 0..4 {
            let c = self.tmr_cell(0x0c + 4 * i as u64);
            if c != self.tmr_compares[i] {
                self.tmr_crossed[i] = false;
            }
            self.tmr_compares[i] = c;
        }
        let cs = self.tmr_cell(0x00);
        if cs != self.tmr_last_cs {
            self.tmr_pending &= cs & 0xf;
        }
        if self.vt_ips != 0 {
            self.vt_us += chunk_insns * 1_000_000 / self.vt_ips;
            // Arch-timer replica of the facade's float virtual clock
            // (same op order, so floor() agrees bit-for-bit).
            self.vt_us_f += (chunk_insns as f64 / self.vt_ips as f64) * 1e6;
            self.cntpct = (self.vt_us_f * 19.2).floor() as u64;
        }
    }

    pub fn fetch(&self, pc: u64) -> Result<u32, Fault> {
        let pc = self.translate(pc)?;
        if Self::is_ram(pc, 4) {
            let a = pc as usize;
            return Ok(u32::from_le_bytes([
                self.mem[a],
                self.mem[a + 1],
                self.mem[a + 2],
                self.mem[a + 3],
            ]));
        }
        Err(Fault::UnmappedFetch(pc))
    }
}

fn mask(size: u64) -> u64 {
    match size {
        1 => 0xff,
        2 => 0xffff,
        4 => 0xffff_ffff,
        _ => u64::MAX,
    }
}

pub struct Cpu {
    pub x: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    /// 128-bit NEON file (moves only — LDR/STR/STP/LDP Q; arithmetic
    /// still faults). Spills in exception-print paths need it.
    pub q: [u128; 32],
    /// VBAR_EL1 + DAIF.I: recorded from MSR, consulted by IRQ delivery
    /// (the `run` harness delivers host-assisted VBAR+0x280 entries).
    /// DAIF.I resets masked, like hardware.
    pub vbar_el1: u64,
    /// DAIF as a 4-bit field (bit3=D, bit2=A, bit1=I, bit0=F), reset
    /// masked like hardware. Guests only ever clear/set I (#2), so D/A/F
    /// stay set — visible in SPSR snapshots all diffs must match.
    pub daif: u8,
    /// ELR_EL1/SPSR_EL1 for real exception entry (lirq-style guests read
    /// them in the vector glue and eret back).
    pub elr_el1: u64,
    pub spsr_el1: u64,
    n: bool,
    z: bool,
    c: bool,
    v: bool,
}

impl Cpu {
    pub fn new(entry: u64) -> Self {
        Cpu { x: [0; 31], sp: 0, pc: entry, q: [0; 32], vbar_el1: 0, daif: 0xf, elr_el1: 0, spsr_el1: 0, n: false, z: false, c: false, v: false }
    }

    #[inline]
    fn r(&self, r: u32) -> u64 {
        if r == 31 { 0 } else { self.x[r as usize] }
    }
    #[inline]
    fn rsp(&self, r: u32) -> u64 {
        if r == 31 { self.sp } else { self.x[r as usize] }
    }
    #[inline]
    fn w(&mut self, r: u32, v: u64, sf: bool) {
        let v = if sf { v } else { v & 0xffff_ffff };
        if r == 31 {
            return;
        }
        self.x[r as usize] = v;
    }
    #[inline]
    fn wsp(&mut self, r: u32, v: u64, sf: bool) {
        let v = if sf { v } else { v & 0xffff_ffff };
        if r == 31 {
            self.sp = v;
        } else {
            self.x[r as usize] = v;
        }
    }

    fn add_with_carry(sf: bool, a: u64, b: u64, carry: u64) -> (u64, bool, bool, bool, bool) {
        if sf {
            let (r1, o1) = a.overflowing_add(b);
            let (r2, o2) = r1.overflowing_add(carry);
            let sa = a as i64;
            let sb = b as i64;
            let sc = carry as i64;
            let (s1, p1) = sa.overflowing_add(sb);
            let (_s2, p2) = s1.overflowing_add(sc);
            let r = r2;
            (r, r >> 63 != 0, r == 0, o1 || o2, p1 || p2)
        } else {
            let a = a as u32 as u64;
            let b = b as u64;
            let (r1, o1) = (a as u32).overflowing_add(b as u32);
            let (r2, o2) = r1.overflowing_add(carry as u32);
            let sa = a as u32 as i32;
            let (s1, p1) = sa.overflowing_add(b as u32 as i32);
            let (_s2, p2) = s1.overflowing_add(carry as i32);
            let r = r2 as u64;
            (r, r >> 31 != 0, r == 0, o1 || o2, p1 || p2)
        }
    }

    fn cond_holds(&self, cond: u32) -> bool {
        match cond {
            0x0 => self.z,                                   // EQ
            0x1 => !self.z,                                  // NE
            0x2 => self.c,                                   // CS/HS
            0x3 => !self.c,                                  // CC/LO
            0x4 => self.n,                                   // MI
            0x5 => !self.n,                                  // PL
            0x6 => self.v,                                   // VS
            0x7 => !self.v,                                  // VC
            0x8 => self.c && !self.z,                        // HI
            0x9 => !self.c || self.z,                        // LS
            0xa => self.n == self.v,                         // GE
            0xb => self.n != self.v,                         // LT
            0xc => !self.z && self.n == self.v,              // GT
            0xd => self.z || self.n != self.v,               // LE
            0xe => true,                                     // AL
            _ => true,                                       // NV behaves as AL
        }
    }

    /// PSTATE snapshot for SPSR_EL1 on exception entry (N/Z/C/V + DAIF
    /// + EL1h mode).
    pub fn pstate(&self) -> u64 {
        ((self.n as u64) << 31)
            | ((self.z as u64) << 30)
            | ((self.c as u64) << 29)
            | ((self.v as u64) << 28)
            | ((self.daif as u64) << 6)
            | 0x5
    }

    /// ERET: restore N/Z/C/V + DAIF from SPSR_EL1, resume at ELR_EL1.
    pub fn eret(&mut self) {
        let s = self.spsr_el1;
        self.n = (s >> 31) & 1 != 0;
        self.z = (s >> 30) & 1 != 0;
        self.c = (s >> 29) & 1 != 0;
        self.v = (s >> 28) & 1 != 0;
        self.daif = ((s >> 6) & 0xf) as u8;
        self.pc = self.elr_el1;
    }

    /// IRQ masked (PSTATE.I)?
    pub fn irq_masked(&self) -> bool {
        (self.daif >> 1) & 1 != 0
    }

    /// NZCV flags (differential debugging aid).
    pub fn flags(&self) -> (bool, bool, bool, bool) {
        (self.n, self.z, self.c, self.v)
    }

    /// Set NZCV directly (single-step differential harness only).
    pub fn set_flags(&mut self, n: bool, z: bool, c: bool, v: bool) {
        self.n = n;
        self.z = z;
        self.c = c;
        self.v = v;
    }

    pub fn step(&mut self, bus: &mut Bus) -> Result<(), Fault> {
        let pc = self.pc;
        let w = bus.fetch(pc)?;
        self.pc = pc.wrapping_add(4);
        self.exec(bus, pc, w)
    }

    pub fn run(&mut self, bus: &mut Bus, budget: u64) -> (u64, Option<Fault>) {
        let mut n = 0u64;
        while n < budget {
            if let Err(f) = self.step(bus) {
                return (n, Some(f));
            }
            n += 1;
        }
        (n, None)
    }

    /// Slice-paced run with timer sync at chunk edges (mirrors the host
    /// slice loop). slice must divide budget for virtual-time runs (else
    /// the tail chunk advances time differently from the host float math).
    pub fn run_sliced(&mut self, bus: &mut Bus, budget: u64, slice: u64) -> (u64, Option<Fault>) {
        let mut n = 0u64;
        while n < budget {
            bus.sync_out();
            let m = core::cmp::min(slice, budget - n);
            let mut done = 0u64;
            let mut fault = None;
            while done < m {
                if let Err(f) = self.step(bus) {
                    fault = Some(f);
                    break;
                }
                done += 1;
            }
            n += done;
            bus.sync_in(done);
            if let Some(f) = fault {
                return (n, Some(f));
            }
        }
        (n, None)
    }
}

// ---- decode helpers ----

#[inline]
fn bits(w: u32, hi: u32, lo: u32) -> u32 {
    (w >> lo) & ((1u32 << (hi - lo + 1)) - 1)
}
#[inline]
fn sext(v: u64, b: u32) -> u64 {
    let sh = 64 - b;
    ((v << sh) as i64 >> sh) as u64
}

// ARM DecodeBitMasks: returns (wmask, tmask, s, r, all_ones) where
// all_ones means s covers the whole element (reserved for
// logical-immediate, valid for LSR/ASR aliases). Callers implementing
// logical-immediate must reject all_ones; bitfield instructions accept it.
fn decode_masks(n: u32, imms: u32, immr: u32, sf: bool) -> Option<(u64, u64, u64, u64, bool)> {
    let cat = ((n & 1) << 6) | ((!imms) & 0x3f);
    if cat == 0 {
        return None;
    }
    let len = 31 - cat.leading_zeros();
    if len < 1 {
        return None;
    }
    // N=1 forces a 64-bit element (and is illegal for 32-bit ops).
    if n == 1 && (len != 6 || !sf) {
        return None;
    }
    let esize: u32 = 1 << len;
    let emask: u64 = if esize >= 64 { u64::MAX } else { (1u64 << esize) - 1 };
    let levels: u64 = (1u64 << len) - 1;
    let s = (imms as u64) & levels;
    let r = (immr as u64) & levels;
    let d = (s + (1u64 << len) - r) & levels;
    let wones: u64 = if s + 1 >= esize as u64 { emask } else { (1u64 << (s + 1)) - 1 };
    let welem = ror(wones, (r % esize as u64) as u32, esize) & emask;
    let telem: u64 = if d + 1 >= esize as u64 { emask } else { (1u64 << (d + 1)) - 1 };
    let mut wmask = 0u64;
    let mut tmask = 0u64;
    let reps = 64 / esize;
    for _ in 0..reps {
        // esize==64 would overflow `<< 64` in debug; single rep = assign.
        wmask = if esize >= 64 { welem } else { (wmask << esize) | welem };
        tmask = if esize >= 64 {
            telem & emask
        } else {
            (tmask << esize) | (telem & emask)
        };
    }
    if !sf {
        wmask &= 0xffff_ffff;
        tmask &= 0xffff_ffff;
    }
    Some((wmask, tmask, s, r, s == levels))
}

fn ror(v: u64, r: u32, size: u32) -> u64 {
    let m = if size >= 64 { u64::MAX } else { (1u64 << size) - 1 };
    let r = r % size;
    if r == 0 {
        return v & m;
    }
    ((v >> r) | (v << (size - r))) & m
}

fn extend_reg(v: u64, option: u32, amount: u32) -> u64 {
    let masked = match option & 7 {
        0b000 => v & 0xff,
        0b001 => v & 0xffff,
        0b010 => v & 0xffff_ffff,
        0b011 => v,
        0b100 => sext(v & 0xff, 8),
        0b101 => sext(v & 0xffff, 16),
        0b110 => sext(v & 0xffff_ffff, 32),
        _ => v,
    };
    masked.wrapping_shl(amount)
}

fn shift_reg(v: u64, kind: u32, amount: u32, sf: bool) -> u64 {
    let m = if sf { u64::MAX } else { 0xffff_ffff };
    match kind {
        0b00 => (v << amount) & m, // LSL
        0b01 => (v & m) >> amount, // LSR
        0b10 => {
            // ASR
            let w = v & m;
            if sf {
                ((w as i64) >> amount) as u64
            } else {
                ((((w as u32) as i32) >> amount) as u32) as u64
            }
        }
        _ => {
            // ROR
            let sz = if sf { 64 } else { 32 };
            ror(v, amount, sz)
        }
    }
}

impl Cpu {
    fn exec(&mut self, bus: &mut Bus, pc: u64, w: u32) -> Result<(), Fault> {
        let ill = Fault::Illegal(w);
        let sf = bits(w, 31, 31) == 1;
        let rn = bits(w, 9, 5);
        let rd = bits(w, 4, 0);

        // B / BL (op = bit31, offset in bits30:26 == 00101)
        if ((w >> 26) & 0x1f) == 0b00101 {
            let imm = sext(((bits(w, 25, 0) as u64) << 2) as u64, 28);
            if bits(w, 31, 31) == 1 {
                self.x[30] = self.pc;
            }
            self.pc = pc.wrapping_add(imm);
            return Ok(());
        }
        // B.cond
        if (w >> 24) == 0x54 {
            let imm = sext(((bits(w, 23, 5) as u64) << 2) as u64, 21);
            if self.cond_holds(bits(w, 4, 0)) {
                self.pc = pc.wrapping_add(imm);
            }
            return Ok(());
        }
        // CBZ / CBNZ
        if ((w >> 25) & 0x3f) == 0x1a {
            let is_cbnz = bits(w, 24, 24) == 1;
            let imm = sext(((bits(w, 23, 5) as u64) << 2) as u64, 21);
            let v = self.r(bits(w, 4, 0));
            let v = if sf { v } else { v & 0xffff_ffff };
            if (v == 0) != is_cbnz {
                self.pc = pc.wrapping_add(imm);
            }
            return Ok(());
        }
        // TBZ / TBNZ
        if ((w >> 25) & 0x3f) == 0x1b {
            let is_tbnz = bits(w, 24, 24) == 1;
            let b40 = bits(w, 23, 19);
            let imm = sext(((bits(w, 18, 5) as u64) << 2) as u64, 16);
            let bit = b40 | (bits(w, 31, 31) << 5);
            let set = ((self.r(bits(w, 4, 0)) >> bit) & 1) == 1;
            if set == is_tbnz {
                self.pc = pc.wrapping_add(imm);
            }
            return Ok(());
        }
        // ERET: exact word only (would otherwise decode as BR XZR -> pc=0).
        if w == 0xD69F03E0 {
            self.eret();
            return Ok(());
        }
        // BR / BLR / RET
        if ((w >> 25) & 0x7f) == 0b1101011 {
            let opc = bits(w, 22, 21);
            let target = self.r(rn);
            match opc {
                0b00 => self.pc = target,
                0b01 => {
                    self.x[30] = self.pc;
                    self.pc = target;
                }
                0b10 => self.pc = target,
                _ => return Err(ill),
            }
            return Ok(());
        }
        // Exception-generating (SVC/HVC/SMC/BRK/...): out of scope.
        if (w >> 24) == 0xD4 {
            return Err(ill);
        }
        // Hints (NOP/ISB/DMB/DSB/...): functional no-ops in this model.
        if (w >> 12) == 0xD5032 || (w >> 12) == 0xD5033 {
            return Ok(());
        }
        if ((w >> 25) & 0x7f) == 0b1101010 {
            // System: VBAR_EL1 + DAIF (IRQ delivery) + the MMU sysregs
            // (TTBR0/TCR/MAIR/SCTLR feed bus translation state).
            // Encodings from assembler truth (`msr vbar_el1, x0`=0xD518C000,
            // `msr daifclr,#2`=0xD50342FF, `msr ttbr0_el1, x0`=0xD5182000,
            // `msr tcr_el1, x1`=0xD5182041, `msr mair_el1, x2`=0xD518A202,
            // `msr sctlr_el1, x3`=0xD5181003):
            // reg move = {L,op0,op1,CRn,CRm,op2} with VBAR={0,3,0,12,0,0};
            // DAIF imm = {op1=3,CRn=4,op2=110(clr)/111(set)}, imm in CRm.
            let l = bits(w, 21, 21);
            let op0 = bits(w, 20, 19);
            let op1 = bits(w, 18, 16);
            let crn = bits(w, 15, 12);
            let crm = bits(w, 11, 8);
            let op2 = bits(w, 7, 5);
            if op0 == 3 && op1 == 0 && crn == 12 && crm == 0 && op2 == 0 {
                if l == 0 {
                    self.vbar_el1 = self.r(rd);
                } else {
                    let v = self.vbar_el1;
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 0 && crm == 0 && (crn == 2 || crn == 1) {
                // TTBR0_EL1 {3,0,2,0,0} / TCR_EL1 {3,0,2,0,2} /
                // SCTLR_EL1 {3,0,1,0,0}: CRn picks the register, op2
                // refines TTBR0 vs TCR (both CRn=2).
                let v = self.r(rd);
                if l == 0 {
                    if crn == 2 && op2 == 0 {
                        bus.mmu_ttbr0 = v;
                    } else if crn == 2 && op2 == 2 {
                        bus.mmu_tcr = v;
                    } else if crn == 1 && op2 == 0 {
                        bus.mmu_sctlr = v;
                    }
                } else if crn == 2 && op2 == 0 {
                    self.w(rd, bus.mmu_ttbr0, true);
                } else if crn == 2 && op2 == 2 {
                    self.w(rd, bus.mmu_tcr, true);
                } else if crn == 1 && op2 == 0 {
                    self.w(rd, bus.mmu_sctlr, true);
                }
            } else if op0 == 3 && op1 == 0 && crn == 10 && crm == 2 && op2 == 0 {
                // MAIR_EL1 {3,0,10,2,0}: recorded, never consulted.
                if l == 0 {
                    bus.mmu_mair = self.r(rd);
                } else {
                    let v = bus.mmu_mair;
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 0 && crn == 4 && crm == 0 && (op2 == 0 || op2 == 1) {
                // SPSR_EL1 {3,0,4,0,0} / ELR_EL1 {3,0,4,0,1} (op2 picks).
                // MRS returns the entry snapshot; MSR is a no-op.
                if l != 0 {
                    let v = if op2 == 0 { self.spsr_el1 } else { self.elr_el1 };
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 3 && crn == 4 && crm == 2 && op2 == 0 {
                // NZCV {3,3,4,2,0} (MRS/MSR differ in L only).
                if l != 0 {
                    let v = ((self.n as u64) << 31)
                        | ((self.z as u64) << 30)
                        | ((self.c as u64) << 29)
                        | ((self.v as u64) << 28);
                    self.w(rd, v, true);
                } else {
                    let v = self.r(rd);
                    self.n = (v >> 31) & 1 != 0;
                    self.z = (v >> 30) & 1 != 0;
                    self.c = (v >> 29) & 1 != 0;
                    self.v = (v >> 28) & 1 != 0;
                }
            } else if op0 == 3 && op1 == 3 && crn == 14 && crm == 0 && op2 == 1 {
                // CNTPCT_EL0 {3,3,14,0,1}: read-only counter.
                if l != 0 {
                    let v = bus.cntpct;
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 3 && crn == 14 && crm == 2 {
                // CNTP_TVAL {..,2,0} / CTL {..,2,1} / CVAL {..,2,2}.
                if l == 0 {
                    let v = self.r(rd);
                    if op2 == 0 {
                        // TVAL: CVAL = counter + value (32-bit offset).
                        bus.cntp_cval = bus.cntpct.wrapping_add(v & 0xffff_ffff);
                    } else if op2 == 1 {
                        bus.cntp_ctl = (v & 1) as u32;
                    } else if op2 == 2 {
                        bus.cntp_cval = v;
                    }
                } else if op2 == 1 {
                    // CTL read: ENABLE bit + ISTATUS (counter >= cval).
                    let mut c = bus.cntp_ctl & 1;
                    if bus.cntpct >= bus.cntp_cval {
                        c |= 1 << 2;
                    }
                    self.w(rd, c as u64, true);
                }
            } else if l == 0 && op1 == 3 && crn == 4 && (op2 & 0b110) == 0b110 {
                // DAIFSet/Clr: imm (CRm, 4 bits D/A/I/F) sets/clears the
                // named bits (op2 bit0: 1=clr, 0=set).
                if (op2 & 1) == 1 {
                    self.daif &= !(crm as u8 & 0xf);
                } else {
                    self.daif |= crm as u8 & 0xf;
                }
            }
            // Anything else (cache ops, PMU, ...): no-op.
            return Ok(());
        }

        // LDP / STP (incl. STNP/LDNP as plain offset, LDPSW). Class is
        // bits(29:25) == 10100 (bit30/31 vary: 64-bit pairs, 32-bit
        // pairs, LDPSW). Verified against assembler truth — never
        // hand-derive these fields again.
        if ((w >> 25) & 0x1f) == 0x14 {
            let is64 = bits(w, 31, 31) == 1;
            // Load-vs-store is bit22 (NOT bit30 — bit30 is 0 for every
            // pair form; the old bit30 test turned all LDPs into STPs,
            // which balanced-stack guests survived self-consistently
            // until the gpio vector glue needed a real restore).
            let is_load = bits(w, 22, 22) == 1;
            // LDPSW (op2 == 11): signed-word pair, 64-bit results.
            // (Machine-derived: regular pairs have op2 == 01.)
            let is_ldpsw = bits(w, 30, 29) == 0b11;
            if is_ldpsw && !is_load {
                return Err(ill);
            }
            let mode = bits(w, 24, 23);
            let sc = if is64 { 8u64 } else { 4u64 };
            let off = sext(bits(w, 21, 15) as u64, 7).wrapping_mul(sc) as i64 as u64;
            let rt2 = bits(w, 14, 10);
            let base = self.rsp(rn);
            // Writeback timing (fork-observed, see test/cpu-cases.mjs):
            // pre-index applies AFTER a successful access (skipped on
            // fault); post-index applies immediately (kept on fault).
            let (addr, wb) = match mode {
                0b10 | 0b00 => (base.wrapping_add(off), None),
                0b11 => {
                    let a = base.wrapping_add(off);
                    (a, Some(a))
                }
                _ => {
                    let a = base;
                    self.wsp(rn, base.wrapping_add(off), true);
                    (a, None)
                }
            };
            if is_load {
                if is_ldpsw {
                    let v1 = bus.read(addr, 4).map_err(|_| Fault::UnmappedData(addr))?;
                    let v2 = bus
                        .read(addr.wrapping_add(4), 4)
                        .map_err(|_| Fault::UnmappedData(addr))?;
                    self.w(rd, sext(v1, 32), true);
                    self.w(rt2, sext(v2, 32), true);
                } else {
                    let v1 = bus.read(addr, sc).map_err(|_| Fault::UnmappedData(addr))?;
                    let v2 = bus
                        .read(addr.wrapping_add(sc), sc)
                        .map_err(|_| Fault::UnmappedData(addr))?;
                    self.w(rd, v1, is64);
                    self.w(rt2, v2, is64);
                }
            } else {
                bus.write(addr, sc, if is64 { self.r(rd) } else { self.r(rd) & 0xffff_ffff })
                    .map_err(|_| Fault::UnmappedData(addr))?;
                let a2 = addr.wrapping_add(sc);
                bus.write(a2, sc, if is64 { self.r(rt2) } else { self.r(rt2) & 0xffff_ffff })
                    .map_err(|_| Fault::UnmappedData(a2))?;
            }
            if let Some(b) = wb {
                self.wsp(rn, b, true);
            }
            return Ok(());
        }
        // SIMD Q-pair (STP/LDP Q): class bits(29:25) == 10110 (vs 10100
        // for integer pairs). 16-byte elements, modes mirror integer
        // pairs. Only the offset/pre/post forms the firmware's spills
        // use; anything else in this class faults.
        if ((w >> 25) & 0x1f) == 0x16 {
            let is_load = bits(w, 22, 22) == 1;
            let mode = bits(w, 24, 23);
            let off = (sext(bits(w, 21, 15) as u64, 7) as i64).wrapping_mul(16) as u64;
            let rt2 = bits(w, 14, 10);
            let base = self.rsp(rn);
            let (addr, wb) = match mode {
                0b10 | 0b00 => (base.wrapping_add(off), None),
                0b11 => {
                    let a = base.wrapping_add(off);
                    (a, Some(a))
                }
                _ => {
                    let a = base;
                    self.wsp(rn, base.wrapping_add(off), true);
                    (a, None)
                }
            };
            if is_load {
                let lo1 = bus.read(addr, 8).map_err(|_| Fault::UnmappedData(addr))?;
                let hi1 = bus
                    .read(addr.wrapping_add(8), 8)
                    .map_err(|_| Fault::UnmappedData(addr))?;
                let lo2 = bus.read(addr.wrapping_add(16), 8).map_err(|_| Fault::UnmappedData(addr))?;
                let hi2 = bus
                    .read(addr.wrapping_add(24), 8)
                    .map_err(|_| Fault::UnmappedData(addr))?;
                self.q[rd as usize] = ((hi1 as u128) << 64) | lo1 as u128;
                self.q[rt2 as usize] = ((hi2 as u128) << 64) | lo2 as u128;
            } else {
                let (l1, h1) = (self.q[rd as usize] as u64, (self.q[rd as usize] >> 64) as u64);
                let (l2, h2) = (self.q[rt2 as usize] as u64, (self.q[rt2 as usize] >> 64) as u64);
                bus.write(addr, 8, l1).map_err(|_| Fault::UnmappedData(addr))?;
                bus.write(addr.wrapping_add(8), 8, h1).map_err(|_| Fault::UnmappedData(addr))?;
                bus.write(addr.wrapping_add(16), 8, l2).map_err(|_| Fault::UnmappedData(addr))?;
                bus.write(addr.wrapping_add(24), 8, h2).map_err(|_| Fault::UnmappedData(addr))?;
            }
            if let Some(b) = wb {
                self.wsp(rn, b, true);
            }
            return Ok(());
        }

        // SIMD single-Q (STR/LDR Q, unsigned-imm/pre/post/unscaled).
        // Class bits(29:25) == 11110 (vs 11100 integer). Only the Q
        // forms the firmware's spills use; D/S/H/B elements,
        // multi-structure, and SIMD arithmetic fault.
        if ((w >> 25) & 0x1f) == 0x1e {
            let opc = bits(w, 23, 22);
            if bits(w, 31, 30) != 0b00 || bits(w, 23, 23) != 1 {
                return Err(ill);
            }
            let is_load = opc & 1 == 1;
            let (addr, wb) = if bits(w, 24, 24) == 1 {
                let off = (bits(w, 21, 10) as u64) * 16;
                (self.rsp(rn).wrapping_add(off), None)
            } else if bits(w, 21, 21) == 1 {
                return Err(ill); // register offset Q (unneeded)
            } else {
                match (bits(w, 11, 11) << 1) | bits(w, 10, 10) {
                    0b00 => {
                        let off = sext(bits(w, 20, 12) as u64, 9);
                        (self.rsp(rn).wrapping_add(off), None)
                    }
                    0b01 => {
                        let off = sext(bits(w, 20, 12) as u64, 9);
                        let b = self.rsp(rn);
                        (b, Some(b.wrapping_add(off)))
                    }
                    0b11 => {
                        let off = sext(bits(w, 20, 12) as u64, 9);
                        let a = self.rsp(rn).wrapping_add(off);
                        (a, Some(a))
                    }
                    _ => return Err(ill),
                }
            };
            if is_load {
                let lo = bus.read(addr, 8).map_err(|_| Fault::UnmappedData(addr))?;
                let hi = bus
                    .read(addr.wrapping_add(8), 8)
                    .map_err(|_| Fault::UnmappedData(addr))?;
                self.q[rd as usize] = ((hi as u128) << 64) | lo as u128;
            } else {
                let (l, h) = (self.q[rd as usize] as u64, (self.q[rd as usize] >> 64) as u64);
                bus.write(addr, 8, l).map_err(|_| Fault::UnmappedData(addr))?;
                bus.write(addr.wrapping_add(8), 8, h).map_err(|_| Fault::UnmappedData(addr))?;
            }
            if let Some(b) = wb {
                self.wsp(rn, b, true);
            }
            return Ok(());
        }

        // STR/LDR (unsigned imm, unscaled, pre/post-index, register offset)
        if ((w >> 25) & 0x1f) == 0x1c {
            let size = bits(w, 31, 30);
            if bits(w, 26, 26) == 1 {
                return Err(ill); // SIMD (non-Q handled above)
            }
            let nbytes = 1u64 << size;
            let opc = bits(w, 23, 22);
            // opc: 00 store, 01 zero-extending load, 10 signed load to
            // 64 bits (LDRSB/H/SW-X), 11 signed load to 32 bits (size<2;
            // unallocated for size>=2). PRFM (size 3, opc 2) faults.
            let is_load = opc != 0b00;
            if size == 3 && opc == 0b10 {
                return Err(ill); // PRFM
            }
            if opc == 0b11 && size >= 2 {
                return Err(ill); // unallocated
            }
            // signed result width in bits (0 = zero-extend/Normal store).
            let sext_to: u32 = match opc {
                0b10 => 64,
                0b11 => 32,
                _ => 0,
            };
            let (addr, wb) = if bits(w, 24, 24) == 1 {
                let off = (bits(w, 21, 10) as u64) * nbytes;
                (self.rsp(rn).wrapping_add(off), None)
            } else {
                // bit24 == 0: bit21 selects register-offset (1) vs imm9
                // modes (0); the latter decode via (bit11,bit10): 00 =
                // unscaled, 01 = post-index, 11 = pre-index (10 is
                // unallocated). Verified word-by-word against the
                // assembler — never hand-derive these fields again.
                if bits(w, 21, 21) == 1 {
                    // register offset
                    let rm = bits(w, 20, 16);
                    let option = bits(w, 15, 13);
                    let s = bits(w, 12, 12);
                    let amount = if s == 1 { size } else { 0 };
                    let off = extend_reg(self.r(rm), option, amount);
                    (self.rsp(rn).wrapping_add(off), None)
                } else {
                    match (bits(w, 11, 11) << 1) | bits(w, 10, 10) {
                        0b00 => {
                            let off = sext(bits(w, 20, 12) as u64, 9);
                            (self.rsp(rn).wrapping_add(off), None)
                        }
                        0b01 => {
                            let off = sext(bits(w, 20, 12) as u64, 9);
                            let b = self.rsp(rn);
                            (b, Some(b.wrapping_add(off)))
                        }
                        0b11 => {
                            // Pre-index: writeback applies AFTER a
                            // successful access (the fork observably skips
                            // it on fault; post-index keeps it).
                            let off = sext(bits(w, 20, 12) as u64, 9);
                            let a = self.rsp(rn).wrapping_add(off);
                            (a, Some(a))
                        }
                        _ => return Err(ill),
                    }
                }
            };
            if is_load {
                let v = bus.read(addr, nbytes).map_err(|_| Fault::UnmappedData(addr))?;
                let v = if sext_to != 0 { sext(v, (nbytes * 8) as u32) } else { v };
                // Signed-to-32 results zero-extend into the 64-bit reg;
                // anything else follows the access size (w() masks).
                self.w(rd, v, sext_to == 64 || size == 3);
            } else {
                let v = if size == 3 { self.r(rd) } else { self.r(rd) & mask(nbytes) };
                bus.write(addr, nbytes, v).map_err(|_| Fault::UnmappedData(addr))?;
            }
            if let Some(b) = wb {
                self.wsp(rn, b, true);
            }
            return Ok(());
        }

        // Data-processing (immediate): op0 = bits(28:25). Branches and
        // loads/stores returned above. Verified against assembler output:
        // 1000 = PC-rel (op1 0000) / ADD-SUB-imm (op1 1000);
        // 1001 = move-wide (op1t 01) / logical-imm (op1t 00) /
        //        bitfield (op1t 10), where op1t = bits(24:23) — the low
        //        two op1 bits are N/hw shared with the operand.
        if ((w >> 25) & 0xf) == 0x8 || ((w >> 25) & 0xf) == 0x9 {
            let op0 = bits(w, 28, 25);
            let op1t = bits(w, 24, 23);
            // PC-relative (bit24 == 0) vs ADD/SUB-immediate (bit24 == 1).
            // (sh/imm12 live below bit24, so this split is stable.)
            if op0 == 0b1000 && bits(w, 24, 24) == 0 {
                let op = bits(w, 31, 31);
                let imm = sext(((bits(w, 23, 5) << 2) | bits(w, 30, 29)) as u64, 21);
                if op == 1 {
                    self.w(rd, (pc & !0xfff).wrapping_add(imm.wrapping_shl(12)), true);
                } else {
                    self.w(rd, pc.wrapping_add(imm), true);
                }
                return Ok(());
            }
            // ADD/SUB immediate
            if op0 == 0b1000 && bits(w, 24, 24) == 1 {
                let op = bits(w, 30, 30);
                let s = bits(w, 29, 29) == 1;
                let imm = (bits(w, 21, 10) as u64) << if bits(w, 22, 22) == 1 { 12 } else { 0 };
                // Rn==31 is SP except with S set (CMP/CMN aliases read
                // XZR — verified against the fork: `cmp sp, x0` clears C).
                let a = if s && rn == 31 { self.r(rn) } else { self.rsp(rn) };
                let (r, n, z, c, v) = if op == 0 {
                    Cpu::add_with_carry(sf, a, imm, 0)
                } else {
                    Cpu::add_with_carry(sf, a, !imm, 1)
                };
                if s {
                    self.n = n;
                    self.z = z;
                    self.c = c;
                    self.v = v;
                }
                // CMP/CMN (S set, Rd==31) discard; otherwise Rd==31 is SP.
                if !(s && rd == 31) {
                    self.wsp(rd, r, sf);
                }
                return Ok(());
            }
            // Logical immediate
            if op0 == 0b1001 && op1t == 0b00 {
                let opc = bits(w, 30, 29);
                let n = bits(w, 22, 22);
                let (wmask, _, _, _, all_ones) =
                    decode_masks(n, bits(w, 15, 10), bits(w, 21, 16), sf).ok_or(ill)?;
                if all_ones {
                    return Err(ill);
                }
                let a = self.r(rn);
                let (r, setf) = match opc {
                    0b00 => (a & wmask, false),
                    0b01 => (a | wmask, false),
                    0b10 => (a ^ wmask, false),
                    _ => (a & wmask, true),
                };
                if setf {
                    let r = if sf { r } else { r & 0xffff_ffff };
                    self.n = if sf { r >> 63 != 0 } else { r >> 31 != 0 };
                    self.z = r == 0;
                    self.c = false;
                    self.v = false;
                }
                self.w(rd, r, sf);
                return Ok(());
            }
            // Move wide
            if op0 == 0b1001 && op1t == 0b01 {
                let opc = bits(w, 30, 29);
                let pos = bits(w, 22, 21) * 16;
                let v = (bits(w, 20, 5) as u64) << pos;
                let m = if sf { u64::MAX } else { 0xffff_ffff };
                let r = match opc {
                    0b00 => (!v) & m,
                    0b10 => v,
                    0b11 => (self.r(rd) & !(0xffffu64 << pos)) | v,
                    _ => return Err(ill),
                };
                self.w(rd, r, sf);
                return Ok(());
            }
            // EXTR (extract register, incl. the ROR-imm alias the
            // assembler emits as extr Rd,Rn,Rn,#sh): gate from 4-word
            // assembler truth, disjoint from the bitfield class below
            // (100110 vs 100111 in bits28:23).
            if ((w >> 23) & 0xff) == 0x27 {
                let rm = bits(w, 20, 16);
                let lsb = bits(w, 15, 10);
                let width = if sf { 64 } else { 32 };
                let m = if sf { u64::MAX } else { 0xffff_ffff };
                let a = self.r(rn) & m;
                let b = self.r(rm) & m;
                let r = if lsb == 0 {
                    a
                } else {
                    ((a >> lsb) | (b << (width - lsb))) & m
                };
                self.w(rd, r, sf);
                return Ok(());
            }
            // Bitfield
            if op0 == 0b1001 && op1t == 0b10 {
                let opc = bits(w, 30, 29);
                let nbits = if sf { 64 } else { 32 };
                let r = bits(w, 21, 16) % nbits;
                let s = bits(w, 15, 10) % nbits;
                let src = self.r(rn);
                let result = match opc {
                    0b10 => {
                        // UBFM: degenerate S==n-1 are MOV/LSR aliases
                        if s == nbits - 1 {
                            if r == 0 {
                                src
                            } else {
                                (src & if sf { u64::MAX } else { 0xffff_ffff }) >> r
                            }
                        } else {
                            // General: oracle-verified selector shared
                            // with BFM/SBFM — S < R inserts at position
                            // (wmask), else extracts low (tmask);
                            // UBFM zero-extends (no dst merge).
                            let (wmask, tmask, ds, dr, _) = decode_masks(
                                bits(w, 22, 22),
                                bits(w, 15, 10),
                                bits(w, 21, 16),
                                sf,
                            )
                            .ok_or(ill)?;
                            let tmp = ror(src, dr as u32, nbits);
                            if ds < dr {
                                tmp & wmask
                            } else {
                                tmp & tmask
                            }
                        }
                    }
                    0b00 => {
                        // SBFM: degenerate are MOV/ASR aliases
                        if s == nbits - 1 {
                            if r == 0 {
                                src
                            } else if sf {
                                ((src as i64) >> r) as u64
                            } else {
                                ((((src as u32) as i32) >> r) as u32) as u64
                            }
                        } else {
                            // General: same selector, then sign-extend
                            // from bit d (d = (S-R) mod nbits). Covers
                            // sbfiz/sbfx and the ROR-imm SBFM shapes.
                            let (wmask, tmask, ds, dr, _) = decode_masks(
                                bits(w, 22, 22),
                                bits(w, 15, 10),
                                bits(w, 21, 16),
                                sf,
                            )
                            .ok_or(ill)?;
                            let tmp = ror(src, dr as u32, nbits);
                            let f = if ds < dr { tmp & wmask } else { tmp & tmask };
                            let d = ds.wrapping_sub(dr) & (nbits as u64 - 1);
                            sext(f, (d + 1) as u32)
                        }
                    }
                    0b01 => {
                        // BFM: oracle-verified (30+ points, valid dst):
                        // S < R inserts at position (wmask form), else
                        // extracts low (tmask form). Covers canonical
                        // BFI/BFXIL plus arbitrary (R,S).
                        let dst = self.r(rd);
                        let (wmask, tmask, ds, dr, _) =
                            decode_masks(bits(w, 22, 22), bits(w, 15, 10), bits(w, 21, 16), sf)
                                .ok_or(ill)?;
                        let tmp = ror(src, dr as u32, nbits);
                        if ds < dr {
                            (dst & !wmask) | (tmp & wmask)
                        } else {
                            (dst & !tmask) | (tmp & tmask)
                        }
                    }
                    _ => return Err(ill),
                };
                self.w(rd, result, sf);
                return Ok(());
            }
            return Err(ill);
        }

        // Data-processing (1-source: RBIT/REV16/REV32/REV/CLZ/CLS).
        // True ARM encoding (assembler ground truth — an earlier version
        // had three transcribed words wrong; never hand-copy words):
        // op6 = bits(15:10) selects the op, sf the width. 0=RBIT,
        // 1=REV16, 2=REV32(X)/REV(W), 3=REV(X only), 4=CLZ, 5=CLS.
        // REV32 reverses bytes WITHIN each 32-bit half (bswap32 per half),
        // it does NOT swap the halves (probed on the fork:
        // 0x1122334455667788 -> 0x4433221188776655). CLS counts the run
        // following the top bit (spec; fork-verified both widths incl.
        // negatives). (sf0, op6=3) is unallocated -> Illegal.
        if (w & 0x7FC0_0000) == 0x5AC0_0000 {
            let sf = bits(w, 31, 31) == 1;
            let op6 = bits(w, 15, 10);
            let rn = bits(w, 9, 5);
            let rd = bits(w, 4, 0);
            let v = self.r(rn);
            let result = match op6 {
                0b000000 => {
                    // RBIT: bit-reverse (W form zero-extends via w()).
                    if sf { v.reverse_bits() } else { (v as u32).reverse_bits() as u64 }
                }
                0b000001 => {
                    // REV16: byte-swap within each 16-bit element.
                    if sf {
                        ((v & 0xFF00_FF00_FF00_FF00) >> 8)
                            | ((v & 0x00FF_00FF_00FF_00FF) << 8)
                    } else {
                        (((v as u32) & 0xFF00_FF00) >> 8
                            | (((v as u32) & 0x00FF_00FF) << 8)) as u64
                    }
                }
                0b000010 => {
                    if sf {
                        // REV32 X: bswap32 each half, halves stay in place.
                        (((v >> 32) as u32).swap_bytes() as u64) << 32
                            | ((v & 0xFFFF_FFFF) as u32).swap_bytes() as u64
                    } else {
                        // REV W.
                        (v as u32).swap_bytes() as u64
                    }
                }
                0b000011 => {
                    if !sf {
                        return Err(ill);
                    }
                    v.swap_bytes() // REV X
                }
                0b000100 => {
                    // CLZ.
                    if sf { v.leading_zeros() as u64 } else { (v as u32).leading_zeros() as u64 }
                }
                0b000101 => {
                    // CLS: run of bits following the top bit equal to it.
                    let nbits = if sf { 64 } else { 32 };
                    let s = (v >> (nbits - 1)) & 1;
                    let mut count = 0u64;
                    for i in (0..nbits - 1).rev() {
                        if ((v >> i) & 1) == s {
                            count += 1;
                        } else {
                            break;
                        }
                    }
                    count
                }
                _ => return Err(ill),
            };
            self.w(rd, result, sf);
            return Ok(());
        }

        // Data-processing (register)
        if ((w >> 24) & 0x1f) == 0x0a || ((w >> 24) & 0x1f) == 0x0b {
            let f = (w >> 24) & 0x1f;
            let rm = bits(w, 20, 16);
            if f == 0x0a {
                // Logical (shifted register)
                let opc = bits(w, 30, 29);
                let shift = bits(w, 23, 22);
                let b = shift_reg(self.r(rm), shift, bits(w, 15, 10), sf);
                // N (bit 21) negates the operand: BIC/ORN/EON/BICS.
                // (Width handled by w() on write.)
                let b = if bits(w, 21, 21) == 1 { !b } else { b };
                let a = self.r(rn);
                let (r, setf) = match opc {
                    0b00 => (a & b, false),
                    0b01 => (a | b, false),
                    0b10 => (a ^ b, false),
                    _ => (a & b, true),
                };
                if setf {
                    let r = if sf { r } else { r & 0xffff_ffff };
                    self.n = if sf { r >> 63 != 0 } else { r >> 31 != 0 };
                    self.z = r == 0;
                    self.c = false;
                    self.v = false;
                }
                self.w(rd, r, sf);
                return Ok(());
            }
            // ADD/SUB (shifted or extended register)
            let op = bits(w, 30, 30);
            let s = bits(w, 29, 29) == 1;
            let b = if bits(w, 21, 21) == 0 {
                shift_reg(self.r(rm), bits(w, 23, 22), bits(w, 15, 10), sf)
            } else {
                extend_reg(
                    self.r(rm),
                    bits(w, 15, 13),
                    bits(w, 12, 10) as u32,
                )
            };
            // Rn==31 is SP except with S set (SUBS aliases incl. NEGS
            // read XZR — verified: `subs`-with-Rn=31 negates).
            let a = if s && rn == 31 { self.r(rn) } else { self.rsp(rn) };
            let (r, n, z, c, v) = if op == 0 {
                Cpu::add_with_carry(sf, a, b, 0)
            } else {
                Cpu::add_with_carry(sf, a, !b, 1)
            };
            if s {
                self.n = n;
                self.z = z;
                self.c = c;
                self.v = v;
            }
            if !(s && rd == 31) {
                self.wsp(rd, r, sf);
            }
            return Ok(());
        }
        // Multiply: MADD/MSUB (32/64, o0=bit15), UMULH/SMULH (U/S=bit23).
        // Masks verified word-by-word against assembler output (see M40
        // notes): each drops exactly the bits that vary within its class.
        if (w & 0x7FE00000) == 0x1B000000 {
            let rm = bits(w, 20, 16);
            let ra = bits(w, 14, 10);
            let is_sub = bits(w, 15, 15) == 1;
            let a = self.r(rn);
            let b = self.r(rm);
            let c = self.r(ra);
            let r = if is_sub {
                c.wrapping_sub(a.wrapping_mul(b))
            } else {
                c.wrapping_add(a.wrapping_mul(b))
            };
            self.w(rd, r, sf);
            return Ok(());
        }
        if (w & 0x7F600000) == 0x1B400000 {
            // UMULH / SMULH (64-bit only)
            if !sf {
                return Err(ill);
            }
            let is_signed = bits(w, 23, 23) == 0;
            let rm = bits(w, 20, 16);
            let hi = if is_signed {
                let a = self.r(rn) as i64 as i128;
                let b = self.r(rm) as i64 as i128;
                ((a.wrapping_mul(b) >> 64) & 0xffff_ffff_ffff_ffff) as u64
            } else {
                let rm = bits(w, 20, 16);
                let a = self.r(rn) as u128;
                let b = self.r(rm) as u128;
                (a.wrapping_mul(b) >> 64) as u64
            };
            self.w(rd, hi, true);
            return Ok(());
        }
        // Signed/unsigned multiply-add long: SMADDL/SMSUBL/UMADDL/UMSUBL
        // (sf=1, op54=00, bit22=0, bit21=1; U=bit23, SUB=bit15). Masks
        // from 4-word assembler truth; disjoint from the MADD arm above
        // (bit21=0 there) and the MULH arm below (bit22=1 there).
        if (w & 0xFF600000) == 0x9B200000 {
            let rm = bits(w, 20, 16);
            let ra = bits(w, 14, 10);
            let is_sub = bits(w, 15, 15) == 1;
            let is_unsigned = bits(w, 23, 23) == 1;
            let a = self.r(rn);
            let b = self.r(rm);
            let c = self.r(ra);
            let (x, y) = if is_unsigned {
                ((a & 0xffff_ffff) as u64, (b & 0xffff_ffff) as u64)
            } else {
                ((a as u32 as i32 as i64 as u64), (b as u32 as i32 as i64 as u64))
            };
            // NOTE: (x as i64) reinterprets correctly for both signs
            // because the extension above already sign/zero-filled.
            let p = ((x as i64).wrapping_mul(y as i64)) as u64;
            let r = if is_sub { c.wrapping_sub(p) } else { c.wrapping_add(p) };
            self.w(rd, r, true);
            return Ok(());
        }
        // Add/subtract with carry: ADC/SBC/ADCS/SBCS (+ NGC/NGCS aliases).
        // Gate from 8-word assembler truth (op=bit30, S=bit29 free).
        if (w & 0x1FE0FC00) == 0x1A000000 {
            let op = bits(w, 30, 30);
            let s = bits(w, 29, 29) == 1;
            let rm = bits(w, 20, 16);
            let a = self.r(rn);
            let b = self.r(rm);
            let c = if self.c { 1 } else { 0 };
            let (r, n, z, cc, v) = if op == 0 {
                Cpu::add_with_carry(sf, a, b, c)
            } else {
                Cpu::add_with_carry(sf, a, !b, c)
            };
            if s {
                self.n = n;
                self.z = z;
                self.c = cc;
                self.v = v;
            }
            self.w(rd, r, sf);
            return Ok(());
        }
        // sf varies — both read inside, never in the mask).
        if (w & 0x3FE00800) == 0x1A800000 {
            let op = bits(w, 30, 30);
            let op2 = bits(w, 10, 10);
            let cond = bits(w, 15, 12);
            let rm = bits(w, 20, 16);
            let a = self.r(rn);
            let b = self.r(rm);
            let t = self.cond_holds(cond);
            let r = match (op << 1) | op2 {
                0b00 => {
                    if t {
                        a
                    } else {
                        b
                    }
                }
                0b01 => {
                    if t {
                        a
                    } else {
                        b.wrapping_add(1)
                    }
                }
                0b10 => {
                    if t {
                        a
                    } else {
                        !b
                    }
                }
                _ => {
                    if t {
                        a
                    } else {
                        (!b).wrapping_add(1)
                    }
                }
            };
            self.w(rd, r, sf);
            return Ok(());
        }
        // Conditional compare: CCMN/CCMP (op=bit30 varies)
        if (w & 0x3FE00000) == 0x3A400000 {
            let op = bits(w, 30, 30);
            let cond = bits(w, 15, 12);
            let nzcv = bits(w, 4, 0);
            let rm = bits(w, 20, 16);
            if self.cond_holds(cond) {
                // CCMN/CCMP never take SP (assembler rejects it): Rn==31
                // reads XZR.
                let a = self.r(rn);
                let b = self.r(rm);
                let (_, n, z, c, v) = if op == 0 {
                    Cpu::add_with_carry(sf, a, b, 0)
                } else {
                    Cpu::add_with_carry(sf, a, !b, 1)
                };
                self.n = n;
                self.z = z;
                self.c = c;
                self.v = v;
            } else {
                self.n = nzcv & 8 != 0;
                self.z = nzcv & 4 != 0;
                self.c = nzcv & 2 != 0;
                self.v = nzcv & 1 != 0;
            }
            return Ok(());
        }
        // Data-processing (2-source): UDIV/SDIV/LSLV/LSRV/ASRV/RORV
        // (sf and the exact opcode bit vary; op2 matched inside).
        // Mask admits opcode bits 15:14=00 + bit12=0 (div 00001x and
        // shift 00101x); the old 0x7FE0F800 fixed bits 15:11=00001
        // (div-only) and silently excluded every shift (dead LSLV/LSRV/
        // ASRV/RORV arms — gpio's `lsl w9,w23,w9` fell to Illegal).
        if (w & 0x7FE0D000) == 0x1AC00000 {
            let op2 = bits(w, 15, 10);
            let rm = bits(w, 20, 16);
            let a = self.r(rn);
            let b = self.r(rm);
            let r = match op2 {
                0b000010 => {
                    // UDIV (divide by zero gives zero, no fault)
                    let bz = b & mask(if sf { 8 } else { 4 });
                    if bz == 0 {
                        0
                    } else if sf {
                        a / b
                    } else {
                        (a as u32 / b as u32) as u64
                    }
                }
                0b000011 => {
                    // SDIV
                    let az = a & mask(if sf { 8 } else { 4 });
                    let bz = b & mask(if sf { 8 } else { 4 });
                    if bz == 0 {
                        0
                    } else if sf {
                        const MIN: i64 = i64::MIN;
                        if (a as i64) == MIN && (b as i64) == -1 {
                            a
                        } else {
                            ((a as i64).wrapping_div(b as i64)) as u64
                        }
                    } else {
                        let aa = (az as u32) as i32;
                        let bb = (bz as u32) as i32;
                        if aa == i32::MIN && bb == -1 {
                            az
                        } else {
                            (aa.wrapping_div(bb) as u32) as u64
                        }
                    }
                }
                0b001000 => {
                    // LSLV
                    let sh = (b & (if sf { 63 } else { 31 })) as u32;
                    (a << sh) & mask(if sf { 8 } else { 4 })
                }
                0b001001 => {
                    // LSRV
                    let sh = (b & (if sf { 63 } else { 31 })) as u32;
                    (a & mask(if sf { 8 } else { 4 })) >> sh
                }
                0b001010 => {
                    // ASRV
                    let sh = (b & (if sf { 63 } else { 31 })) as u32;
                    if sf {
                        ((a as i64) >> sh) as u64
                    } else {
                        ((((a as u32) as i32) >> sh) as u32) as u64
                    }
                }
                0b001011 => {
                    // RORV
                    let sz = if sf { 64 } else { 32 };
                    ror(a, (b & (sz as u64 - 1)) as u32, sz)
                }
                _ => return Err(ill),
            };
            self.w(rd, r, sf);
            return Ok(());
        }
        Err(ill)
    }
}

// ---- minimal ELF64-LE loader (PT_LOAD only) ----

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes([
        b[o], b[o + 1], b[o + 2], b[o + 3], b[o + 4], b[o + 5], b[o + 6], b[o + 7],
    ])
}

/// Load PT_LOAD segments at their p_vaddrs. Returns the entry point.
pub fn load_elf(bus: &mut Bus, bytes: &[u8]) -> Result<u64, String> {
    if bytes.len() < 64
        || &bytes[0..4] != b"\x7fELF"
        || bytes[4] != 2
        || bytes[5] != 1
        || u16le(bytes, 16) != 2
        || u16le(bytes, 18) != 183
    {
        return Err("not a 64-bit LE AArch64 ET_EXEC".into());
    }
    let entry = u64le(bytes, 24);
    let phoff = u64le(bytes, 32) as usize;
    let phentsize = u16le(bytes, 54) as usize;
    let phnum = u16le(bytes, 56) as usize;
    for i in 0..phnum {
        let o = phoff + i * phentsize;
        if u32le(bytes, o) != 1 {
            continue; // PT_LOAD only
        }
        let off = u64le(bytes, o + 8) as usize;
        let vaddr = u64le(bytes, o + 16);
        let filesz = u64le(bytes, o + 32) as usize;
        let memsz = u64le(bytes, o + 40) as usize;
        if memsz < filesz || off + filesz > bytes.len() {
            return Err("bad segment".into());
        }
        for j in 0..filesz {
            bus.write(vaddr + j as u64, 1, bytes[off + j] as u64)
                .map_err(|_| "segment outside RAM".to_string())?;
        }
        for j in filesz..memsz {
            bus.write(vaddr + j as u64, 1, 0)
                .map_err(|_| "segment outside RAM".to_string())?;
        }
    }
    Ok(entry)
}

