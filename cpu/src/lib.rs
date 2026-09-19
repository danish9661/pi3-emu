// pi-cpu: a small AArch64 interpreter for the pi3-emu bare-metal guests.
//
// Scope (M40 spike): integer + control flow + loads/stores, flat memory,
// zero-mapped MMIO with a PL011-TX console tap. No FP/NEON, no MMU, no
// exceptions — anything outside the subset faults cleanly (Fault) so the
// differential harness against unicorn.js catches coverage gaps instead of
// silently diverging.
//
// Endianness: little-endian throughout.

pub mod runner;
#[cfg(target_arch = "wasm32")]
pub mod wasm;

pub const RAM_SIZE: u64 = 0x400000;
// M56 Linux track: 512 MB guest RAM (matches qemu raspi3ap `-m 512M`).
// The 4 MB default cannot hold the 22 MB kernel8.img + DTB + initramfs.
// Expanded conditionally at load: `ram_size()` returns 512M once
// `linux_mode` is set by `load_linux()`, else the legacy 4M (all goldens,
// fuzzers, upython suites, and the wasm demo keep exact legacy behavior).
pub const LINUX_RAM_SIZE: u64 = 0x20000000;
pub const UART0: u64 = 0x3f201000;
pub const TMR_BASE: u64 = 0x3f003000;
pub const GPIO_BASE: u64 = 0x3f200000;
pub const UART1_BASE: u64 = 0x3f215000; // mini UART (TX tap only)
pub const I2C_BASE: u64 = 0x3f804000; // BSC master + host sensor slave
pub const SPI_BASE: u64 = 0x3f204000; // SPI0 master + host flash slave
pub const MBOX_BASE: u64 = 0x3f00b880; // VideoCore mailbox + framebuffer
pub const MMU_CTL: u64 = 0x3f00d000; // host-assisted MMU window (compat)
pub const DMA_BASE: u64 = 0x3f007000; // DMA ch0 + ENABLE extension
pub const DMA_ENABLE_PAGE: u64 = 0x3f00e000; // ENABLE lives here (facade maps the page)
pub const PWM_BASE: u64 = 0x3f20c000; // PWM FIFO-mode + audio samples
pub const SMP_BASE: u64 = 0x3f202000; // SMP spin-table mailbox (host-arbitrated)
pub const SD_BASE: u64 = 0x3f300000;
// M30 windows (periphs/debug guests): HW RNG + temp (shared block),
// clock manager, I2S/PCM, BSC0, AUX mini-UARTs 2-5, DWC2 USB (SNPSID +
// DONE only — no full OTG model).
pub const RNG_BASE: u64 = 0x3f104000;
pub const CLK_BASE: u64 = 0x3f100000;
pub const I2S_BASE: u64 = 0x3f203000;
pub const I2C0_BASE: u64 = 0x3f205000;
pub const UART2_BASE: u64 = 0x3f216000;
pub const UART3_BASE: u64 = 0x3f217000;
pub const UART4_BASE: u64 = 0x3f218000;
pub const UART5_BASE: u64 = 0x3f219000;
pub const USB_BASE: u64 = 0x3f980000;
pub const USB_LEN: u64 = 0x40000;
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
    /// M56 Linux track: once `load_linux()` runs, RAM expands to 512M
    /// and `ram_size()` (not the `RAM_SIZE` const) gates every range
    /// check. New code must use `in_ram()`; legacy `is_ram()` stays
    /// for the 4M goldens. Public: the runner's M61 cntp gate needs it
    /// (bare-metal guests keep raw-compare delivery; Linux needs the
    /// local-enable gate).
    pub linux_mode: bool,
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
    /// GPFSEL0-5 backing (real BCM2837: function-select is R/W; the
    /// old facade echoed it via window RAM, so guests/tests read back
    /// what they wrote).
    gpio_fsel: [u32; 6],
    // EV-reg cells indexed (off-0x4C)/4 over 0x4C..=0x8C (reserved gaps
    // included as harmless cells, like window memory); pair p bank b
    // lives at [0,3,6,9,12,15][p]+b (REN/FEN/HEN/LEN/AREN/AFEN).
    gpio_en: [u32; 17],
    // Legacy IC (0x3F00B200 — mirrors ic.js + upstream irq-bcm2835.c):
    // ENABLE accumulates, DISABLE clears, PENDING reads show line&enabled.
    // Register layout (upstream reg_enable[] = {0x18, 0x10, 0x14},
    // reg_disable[] = {0x24, 0x1C, 0x20}: bank-0 enable is at +0x18,
    // NOT +0x10 — the old code had no bank-0 enable at all, so the
    // mailbox driver's bank-0 bit-1 enable never latched and the MAIL0
    // IRQ line never raised: every firmware transaction timed out at
    // 3.4s, proven by ICWR-trace showing only +0x10/+0x14 writes).
    ic_en0: u32,
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
    /// M60 MMIO-write census (Linux bring-up oracle, zero-cost when off):
    /// counts guest writes per device window (UART/TMR/GPIO/IC/MBOX/SD/
    /// LOCAL/MINIUART/I2C/SPI/PWM/SMP/MMU/RNG+/USB), incremented in
    /// `write()` only while `mmio_census` is set. Proves device silence
    /// by execution (e.g. "UART MMIO writes == 0 at 21M" = kernel has
    /// not reached earlycon, not "hook missed it").
    pub mmio_census: bool,
    pub census_uart: u64,
    pub census_tmr: u64,
    pub census_gpio: u64,
    pub census_ic: u64,
    pub census_mbox: u64,
    pub census_sd: u64,
    pub census_local: u64,
    pub census_miniuart: u64,
    pub census_i2c: u64,
    pub census_spi: u64,
    pub census_pwm: u64,
    pub census_smp: u64,
    pub census_mmu: u64,
    pub census_misc: u64,
    // Stage-1 MMU state (EL1, 4K granule). Written by the Cpu's MSR arm
    // (translation runs inside bus.read/write/fetch, which need them
    // there). Permissions/AF/MAIR are NOT modeled — everything the
    // guests map is full-access Normal-equivalent RAM/MMIO.
    pub mmu_sctlr: u64,
    pub mmu_tcr: u64,
    pub mmu_ttbr0: u64,
    pub mmu_ttbr1: u64,
    pub mmu_mair: u64,
    // ARM arch timer (CNTP + CNTV, 19.2 MHz like the Pi 3): the counter follows
    // the facade's float virtual-time replica exactly (virtualUs_f +=
    // (n/ips)*1e6 per chunk, cntpct = floor(us*19.2)) so compare matches
    // fire on the same chunk on both sides. TVAL writes latch
    // cval = cntpct + val; CTL bit 0 enables. CNTV (virtual timer,
    // {..,14,3,..}) has INDEPENDENT backing (M60: the kernel programs
    // CNTV first at +145.9M; sharing one cval/ctl made CNTV's disable
    // clobber CNTP's later program — proven by execution: cntp_line
    // stuck true from 145908736 with DAIF set, faulting the smp-
    // processor-id slow path at 149748056).
    pub cntpct: u64,
    pub cntp_cval: u64,
    pub cntp_ctl: u32,
    cntv_cval: u64,
    cntv_ctl: u32,
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
    /// DONE host-extension latch (guest writes SD+0x54, like TMR_DONE):
    /// the write is absorbed (no readable cell), so the flag carries it.
    sd_done: bool,
    // AUX mini UART (0x3F215000 — mirrors uart1.js): window backing
    // (the facade's window IS RAM: guest writes persist, the host
    // overwrites ENABLES/LSR/IO every slice), enable latch, TX pulse
    // cell, and the "[u1] " line-tag state.
    uart1_back: [u32; 64],
    uart1_enabled: bool,
    uart1_line_start: bool,
    // M30 windows (periphs/debug guests — mirrors rng.js, clockmgr.js,
    // i2s.js, uart25.js, usb.js): RNG CTRL latch (DATA reads a fixed
    // 45.0 C like the temp model), AUX mini-UART 2-5 window backing +
    // enable latches (LSR served live), USB DWC2 OTG core (M67: full
    // register file + host channels + IRQ line; see usb_* fields).
    // CLK/I2S/I2C0 are zero windows (absorbing, like the
    // facade's untouched windows).
    rng_ctrl: u32,
    uart25_back: [[u32; 64]; 4],
    uart25_enabled: [bool; 4],
    usb_done: bool,
    // M67 DWC2 OTG core state (0x3F980000, 0x11000 bytes — mirrors
    // QEMU hcd-dwc2.c reset values + Linux drivers/usb/dwc2/hw.h bit
    // defs, and ports the old facade src/usb.js state machine):
    // - glb: GOTGCTL/GOTGINT/GAHBCFG/GUSBCFG/GRSTCTL/GINTSTS/GINTMSK/
    //   GRXFSIZ/GNPTXFSIZ/GNPTXSTS/GI2CCTL/GPVNDCTL/GGPIO/GUID/GSNPSID/
    //   GHWCFG1-4/GLPMCFG/GPWRDN/GDFIFOCFG/GADPCTL shadow cells.
    // - host: HPTXFSIZ/HCFG/HFIR/HFNUM/HPTXSTS/HAINT/HAINTMSK/HPRT0.
    // - 8 host channels (DWC2_NB_CHAN=8): HCCHAR/HCSPLT/HCINT/HCINTMSK/
    //   HCTSIZ/HCDMA/HCDMAB cells (HCTSIZ/HCDMA latched for DMA).
    // - frame counter + SOF throttle (HFNUM advances per sync_out;
    //   SOF bit pulses every 8th tick like the facade).
    // Guests verified against this model: periphs/debug (GSNPSID),
    // usb/eth demo guests (core bring-up + IRQ + channels), and the
    // Linux dwc2 driver (probe without the initcall blacklist).
    usb_glb: [u32; 28],
    usb_hreg0: [u32; 17],
    usb_hch: [[u32; 8]; 8],
    usb_frame: u32,
    usb_sof_ticks: u32,
    usb_hprt_conn: bool,
    /// LAN7800 (Pi 3 B+ onboard ethernet, usb424:7800 on the DTB's
    /// usb-port@1 tree): USB-device-side model behind the DWC2 host.
    /// Fixed MAC b8:27:eb:de:ad:be (matches the 0x10003 mailbox tag
    /// reply + DTB-less firmware default), link always up 1000 Mb/s
    /// full duplex, RX queue fed by the harness (browser), TX queue
    /// drained to the harness. Driver-visible via standard USB
    /// control/bulk transfers on the host channels (see usb_xfer).
    usb_mac: [u8; 6],
    usb_link_up: bool,
    usb_rx: Vec<u8>,
    usb_tx: Vec<u8>,
    /// Pending control SETUP packet (latched by the EP0/OUT xfer==8
    /// setup-stage absorb above; consumed by the following EP0
    /// data/status stage). Real stacks move setup through the FIFO;
    /// pi-cpu has no FIFO model, so the latch carries it.
    usb_setup: [u8; 8],
    usb_setup_valid: bool,
    /// Last transmitted frame (bulk-OUT payload) for the eth-guest
    /// loopback read (see the IN arm above). Real hardware would put
    /// it on the wire; with no harness input queued, echoing it lets
    /// the demo verify the full TX->RX path deterministically.
    usb_loopback: Vec<u8>,
    // I2C window backing (DLEN/A/FIFO/DONE cells; C/S are computed).
    // I2C window backing (DLEN/A/FIFO/DONE cells; C/S are computed).
    i2c_back: [u32; 32],
    i2c_pub_c: u32,
    i2c_pub_s: u32,
    spi_pub_cs: u32,
    // I2C/BSC (0x3F804000 — mirrors i2c.js): ST rising edge (detected
    // synchronously on C writes, equivalent to the facade's slice
    // diff), slave register + 0x68 sensor, 4-byte FIFO backing.
    i2c_c: u32,
    i2c_sdone: bool,
    i2c_dlen: u32,
    i2c_addr: u32,
    i2c_counter: u32,
    i2c_reg: u32,
    i2c_resp: [u8; 4],
    i2c_fifo: [u8; 4],
    // SPI0 (0x3F204000 — mirrors spi.js): TX/RX queues with the JEDEC
    // flash slave, TA edge handling on CS writes, 4-byte FIFO backing.
    spi_tx: Vec<u8>,
    spi_rx: Vec<u8>,
    spi_cmd: u8,
    spi_ta: bool,
    spi_sdone: bool,
    spi_fifo: [u8; 4],
    // Last guest CS write since publish (the facade window is RAM: a
    // same-slice write-then-read observes the written value, e.g. the
    // TA write followed by the DONE poll; cleared on publish).
    spi_cs_dirty: Option<u32>,
    /// DONE host-extension latch (guest writes SPI+0x54, like TMR_DONE):
    /// absorbed like other unlisted cells, so the flag carries it.
    spi_done: bool,
    // VideoCore mailbox (0x3F00B880 — mirrors main.js mboxProcess +
    // fbTag): multi-shot property requests (the fb guest sends all six
    // tags in one buffer; the kernel sends one request per driver probe,
    // M61: no changed-value gate — the kernel reuses one buffer address
    // per probe — and mbx_pending (the publish flag) drives the STATUS
    // snapshots. The kernel-path STATUS cells (+0x18 MAIL0_STA / +0x38
    // MAIL1_STA) serve the live EMPTY bit; the legacy cells (+0x04 /
    // always-clear +0x18-legacy) keep the fb/shell/debug goldens pinned.
    // First-cut traps, all execution-proven: (1) the +0x14 MAIL0_SENDER
    // latch never saw the kernel's +0x20 writes (mbox census ~1 while
    // the driver timed out); (2) the MAIL1 word is a VC BUS address
    // (mask 0x3FFFFFFF: 0xdc02/060008 -> PA 0x1c02/060000); (3) the
    // hardwired +0x38=0 meant EMPTY-always so the driver's MAIL1_STA
    // poll never saw its reply collected. Framebuffer geometry exposed
    // for the browser canvas blit. mbx_* cells are pub for the M61
    // triage probe (mailbox buffer dump).
    pub mbx_pending: bool,
    pub mbx_addr: u32,
    pub mbx_last_write: u32,
    pub mbx_pub_read: u32,
    pub mbx_pub_status: u32,
    // MAIL0_CNF interrupt-enable latch (M61 IRQ path): the mailbox
    // driver writes IHAVEDATAIRQEN (bit 0) at probe (bcm2835_startup)
    // and clears it at shutdown; while set, a pending reply raises the
    // bank-0 bit-1 line so the IRQ handler drains MAIL0_RD (the real
    // completion path — the driver never polls STATUS).
    mbx_cnf_irqen: bool,
    /// M61 drain-watch edge state (see Runner::mbox_drains): last gated
    /// mailbox-line level observed at a chunk boundary. Only maintained
    /// while `mbox_drain_log` is set; the runner owns the log itself.
    pub mbox_prev_live: bool,
    /// M61 drain-watch enable (zero-cost when off): set by triage probes
    /// that need proof of MAIL0 drains by execution.
    pub mbox_drain_log: bool,
    /// M63 PA write-watch (zero-cost unless armed): when `wwatch_on` is
    /// set, any RAM store overlapping `wwatch_pa..+wwatch_len` emits a
    /// `WWATCH` trace line with the store's own `size`/`val` and the
    /// value already there. The CALLER (Cpu::step, which owns the guest
    /// pc) appends the pc — see the `WWATCH` site there.
    /// PROVEN (ww.err 2B run, 2.75M hits): PA 0x1e28008 = [sp_el0+8] is
    /// the shared completion word — the weigh/wake pair from the stall
    /// loop (`...0c1828 sub+str` / `...0c28b0 add+str`) plus sibling
    /// sites with the IDENTICAL `ldr w1,[x0,#8] / add w1,w1,#1` /
    /// `str w1,[x0,#8]` shape (`...1c0b48/68/ec/c00`, `...1c0774/94/
    /// d0/e8`). The advancer is normal guest code, NOT the mailbox IRQ
    /// handler (ICRD=0 everywhere) — so no handler exists to complete
    /// inline in mbox_process; the completion must come from the
    /// already-written synchronous reply being OBSERVED (see the
    /// M63 note on mbox_process).
    /// `wwatch_hits` counts watch log lines so step emits the pc tag
    /// ONLY on steps that actually hit (exact volume, no flood).
    pub wwatch_on: bool,
    pub wwatch_pa: u64,
    pub wwatch_len: u64,
    pub wwatch_hits: u64,
    // Local-block per-core timer/mailbox control cells (M61 IRQ path):
    // LOCAL_TIMER_INT_CONTROL0 (+0x40, core 0): low 4 bits = per-core
    // arch-timer IRQ enables (bit1 = CNTPNSIRQ — the timer the kernel's
    // clocksource tick uses); LOCAL_MAILBOX_INT_CONTROL0 (+0x50, core 0):
    // low 4 bits = per-mailbox IRQ enables (unused by the mailbox
    // driver — its completion arrives via the legacy IC bank-0 GPU
    // chain, not the local mailbox lines — so latched, never consulted).
    // Upstream: irq-bcm2836.c. Unknown bits absorbed (FIQ halves, core
    // 1..3 strides — single core 0 only).
    local_timer_ctl0: u32,
    local_mbox_ctl0: u32,
    // Local-block GPU routing latch (M61 IRQ path): GPU_ROUTING at
    // LOCAL+0x0C (upstream ARM_LOCAL_GPU_INT_ROUTING; the kernel writes
    // 0x0 at boot — proven by LOCALWR trace — which routes GPU IRQs to
    // the legacy IC's bank-0 path that the chained handler serves).
    // Nonzero would steer them elsewhere (FIQ/local); only 0x0 is
    // modeled, other values are latched and ignored.
    local_gpu_routing: u32,
    fb_w: u32,
    fb_h: u32,
    fb_depth: u32,
    fb_pitch: u32,
    fb_ready: bool,
    // DMA ch0 (0x3F007000 + ENABLE page — mirrors main.js syncDmaOut/In
    // + dma.js): full window backing (the facade windows ARE RAM:
    // guest writes persist and read back until sync overwrites); END/INT
    // latch on chain completion at sync_in; CS reads serve backing (the
    // publish overwrites it every sync_out, so ACTIVE never echoes back
    // past a boundary — same as the facade).
    dma_back: [u32; 256],
    dma_en_back: [u32; 256],
    dma_end: bool,
    dma_int: bool,
    dma_last_cs: u32,
    // MMU_CTL compat window (0x3F00D000 — mirrors the host-assisted
    // model for the mmu guest): writing root|1 programs the REAL
    // stage-1 regime (TTBR0=root, T0SZ=16 48-bit 4K, MAIR, SCTLR.M)
    // so the guest's tables translate natively; reads echo the cell
    // (the guest polls bit0, set by its own write).
    mmu_ctl_cell: u32,
    mmu_done_cell: u32,
    /// Facade-dialect tables (MMU_CTL regime): 0b01 descends like a
    /// table at L0/L1/L2 (the mmu guest links tables with 0b01, which
    /// strict ARM reads as L1/L2 blocks). Native TTBR0 regimes (mva)
    /// keep strict decoding. Set by MMU_CTL-enable.
    mmu_loose: bool,
    // SMP spin-table mailbox (0x3F202000 — host-arbitrated window like
    // main.js smpState): plain backing; the SmpShared arbiter mirrors
    // it per core per chunk (see runner.rs).
    smp_mem: [u8; 0x1000],
    // PWM (0x3F20C000 — mirrors pwm.js FIFO mode): CTL latch + FIFO
    // queue drained 64/chunk into a sample ring (browser audio reads
    // it via pwm_take). STA/FULL/EMPT published at sync_out.
    pwm_back: [u32; 32],
    pwm_ctl: u32,
    pwm_last_ctl: u32,
    pwm_fifo: Vec<u32>,
    pwm_ring: Vec<u32>,
    pwm_drained: u64,
}

impl Bus {
    pub fn new() -> Self {
        let mut b = Bus {
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
            gpio_fsel: [0; 6],
            gpio_en: [0; 17],
            ic_en0: 0,
            ic_en1: 0,
            ic_en2: 0,
            irq_ret_pending: false,
            uart0_cr: 0,
            uart0_lcrh: 0,
            uart0_ibrd: 0,
            uart0_fbrd: 0,
            uart0_imsc: 0,
            uart0_rx: Vec::new(),
            mmio_census: false,
            census_uart: 0,
            census_tmr: 0,
            census_gpio: 0,
            census_ic: 0,
            census_mbox: 0,
            census_sd: 0,
            census_local: 0,
            census_miniuart: 0,
            census_i2c: 0,
            census_spi: 0,
            census_pwm: 0,
            census_smp: 0,
            census_mmu: 0,
            census_misc: 0,
            mmu_sctlr: 0,
            mmu_tcr: 0,
            mmu_ttbr0: 0,
            mmu_ttbr1: 0,
            mmu_mair: 0,
            cntpct: 0,
            cntp_cval: 0,
            cntp_ctl: 0,
            cntv_cval: 0,
            cntv_ctl: 0,
            vt_us_f: 0.0,
            sd_arg: 0,
            sd_cmd: 0,
            sd_resp0: 0,
            sd_stage: [0; 512],
            sd_irpt: 0,
            sd_disk: Bus::make_disk(),
            sd_done: false,
            uart1_back: [0; 64],
            uart1_enabled: false,
            uart1_line_start: true,
            rng_ctrl: 0,
            uart25_back: [[0; 64]; 4],
            uart25_enabled: [false; 4],
            usb_done: false,
            usb_glb: Bus::dwc2_reset_glb(),
            usb_hreg0: Bus::dwc2_reset_hreg0(),
            usb_hch: [[0; 8]; 8],
            usb_frame: 0x3fff,
            usb_sof_ticks: 0,
            usb_hprt_conn: false,
            usb_mac: [0xb8, 0x27, 0xeb, 0xde, 0xad, 0xbe],
            usb_link_up: true,
            usb_rx: Vec::new(),
            usb_tx: Vec::new(),
            usb_setup: [0; 8],
            usb_setup_valid: false,
            usb_loopback: Vec::new(),
            i2c_back: [0; 32],
            i2c_pub_c: 0,
            i2c_pub_s: 0,
            spi_pub_cs: 0,
            i2c_c: 0,
            i2c_sdone: false,
            i2c_dlen: 0,
            i2c_addr: 0,
            i2c_counter: 0,
            i2c_reg: 0,
            i2c_resp: [0; 4],
            i2c_fifo: [0; 4],
            spi_tx: Vec::new(),
            spi_rx: Vec::new(),
            spi_cmd: 0,
            spi_ta: false,
            spi_sdone: false,
            spi_fifo: [0; 4],
            spi_cs_dirty: None,
            spi_done: false,
            mbx_pending: false,
            mbx_addr: 0,
            mbx_last_write: 0,
            mbx_pub_read: 0,
            mbx_pub_status: 0x80000000,
            mbx_cnf_irqen: false,
            mbox_prev_live: false,
            mbox_drain_log: false,
            wwatch_on: false,
            wwatch_pa: 0,
            wwatch_len: 0,
            wwatch_hits: 0,
            local_timer_ctl0: 0,
            local_mbox_ctl0: 0,
            local_gpu_routing: 0,
            fb_w: 0,
            fb_h: 0,
            fb_depth: 0,
            fb_pitch: 0,
            fb_ready: false,
            dma_back: [0; 256],
            dma_en_back: [0; 256],
            dma_end: false,
            dma_int: false,
            dma_last_cs: 0,
            mmu_ctl_cell: 0,
            mmu_done_cell: 0,
            mmu_loose: false,
            smp_mem: [0; 0x1000],
            pwm_back: [0; 32],
            pwm_ctl: 0,
            pwm_last_ctl: 0,
            pwm_fifo: Vec::new(),
            pwm_ring: Vec::new(),
            pwm_drained: 0,
            linux_mode: false,
        };
        b
    }

    /// Active RAM size: 512M in Linux mode, legacy 4M otherwise.
    /// `is_ram`/`translate` consult this so every golden keeps the
    /// legacy map until `load_linux()` expands it.
    pub fn ram_size(&self) -> u64 {
        if self.linux_mode {
            LINUX_RAM_SIZE
        } else {
            RAM_SIZE
        }
    }

    pub fn is_ram(addr: u64, size: u64) -> bool {
        addr.checked_add(size).map_or(false, |e| e <= RAM_SIZE)
    }

    /// Instance RAM check (Linux-mode aware).
    pub fn in_ram(&self, addr: u64, size: u64) -> bool {
        addr.checked_add(size).map_or(false, |e| e <= self.ram_size())
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

    fn is_i2c(addr: u64, size: u64) -> bool {
        Self::is_page(addr, size, I2C_BASE)
    }

    fn is_spi(addr: u64, size: u64) -> bool {
        Self::is_page(addr, size, SPI_BASE)
    }

    fn is_pwm(addr: u64, size: u64) -> bool {
        Self::is_page(addr, size, PWM_BASE)
    }

    fn is_mmuctl(addr: u64, size: u64) -> bool {
        Self::is_page(addr, size, MMU_CTL)
    }

    fn is_smp(addr: u64, size: u64) -> bool {
        Self::is_page(addr, size, SMP_BASE)
    }

    fn is_rng(addr: u64, size: u64) -> bool {
        Self::is_page(addr, size, RNG_BASE)
    }

    fn is_clk(addr: u64, size: u64) -> bool {
        Self::is_page(addr, size, CLK_BASE)
    }

    fn is_i2s(addr: u64, size: u64) -> bool {
        Self::is_page(addr, size, I2S_BASE)
    }

    fn is_i2c0(addr: u64, size: u64) -> bool {
        Self::is_page(addr, size, I2C0_BASE)
    }

    fn is_uart25(addr: u64, size: u64) -> bool {
        Self::is_page(addr, size, UART2_BASE)
            || Self::is_page(addr, size, UART3_BASE)
            || Self::is_page(addr, size, UART4_BASE)
            || Self::is_page(addr, size, UART5_BASE)
    }

    fn is_usb(addr: u64, size: u64) -> bool {
        addr >= USB_BASE && addr.checked_add(size).map_or(false, |e| e <= USB_BASE + USB_LEN)
    }

    /// Mapped device windows (any access size starting inside one). The
    /// host-assisted guests (mmu) leave MMIO unmapped in their tables
    /// and rely on the host bypass (like the facade, whose windows stay
    /// accessible with translation on) — so translation identity-maps
    /// these instead of walking. Unmapped NON-device VAs still fault
    /// (the debug guest depends on it).
    /// M58 hot-path: every fetch/read/write calls this (3× per insn
    /// with MMU on), so it is three range compares — the peripheral
    /// block 0x3F00_0000..0x3F98_0000+USB_LEN plus the local block
    /// page at 0x4000_0000. Cheaper than 19 is_*() calls (each with
    /// checked_add) and exact for the bypass set (translate() only
    /// needs "device vs RAM", never which one; LOCAL page included —
    /// CORE_IRQ_SRC must bypass too). TMR_BASE/DMA_BASE sit just
    /// below MBOX_PAGE (0x3F003000/0x3F007000 < 0x3F00B000), so the
    /// peripheral floor is TMR_BASE, not MBOX_PAGE.
    fn is_device_win(addr: u64) -> bool {
        (addr >= TMR_BASE && addr < USB_BASE + USB_LEN)
            || (addr >= LOCAL_BASE && addr < LOCAL_BASE + 0x1000)
    }

    /// DMA ch0 window cell: CS/CONBLK_AD in the channel window, ENABLE
    /// page (the facade maps the whole 4K page at real layout).
    fn is_dma(addr: u64, size: u64) -> bool {
        (addr >= DMA_BASE && addr.checked_add(size).map_or(false, |e| e <= DMA_BASE + 0x1000))
            || (addr >= DMA_ENABLE_PAGE
                && addr.checked_add(size).map_or(false, |e| e <= DMA_ENABLE_PAGE + 0x1000))
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

    /// Stage-1 VA→PA translation (4K granule, TTBR0+TTBR1). Public
    /// for the M57 triage probe (direct PA check); the CPU/fetch/
    /// read/write paths are the production users. Identity while
    /// SCTLR.M is clear. Mapped device windows bypass the walk
    /// (host extension for host-assisted guests — see is_device_win).
    /// TTBR0 covers the low half (TTBR0 walks when bit55==0, sized by
    /// T0SZ); TTBR1 covers the high half (bit55==1, sized by T1SZ) —
    /// the M57 Linux-track addition (the kernel's PAGE_OFFSET linear
    /// map lives at 0xffffffc0...). Faults on out-of-range VA, non-4K
    /// granule, bad descriptors, unmapped non-device VAs, or tables
    /// outside RAM.
    pub fn translate(&self, va: u64) -> Result<u64, Fault> {
        if (self.mmu_sctlr & 1) == 0 {
            return Ok(va);
        }
        if Self::is_device_win(va) {
            return Ok(va);
        }
        // High-half select: bit55 (not bit63 — with 39/48-bit VAs the
        // top byte is a sign extension/tag, not the TTBR selector).
        let hi = (va >> 55) & 1 != 0;
        let tsz = if hi {
            ((self.mmu_tcr >> 16) & 0x3f) as u32
        } else {
            (self.mmu_tcr & 0x3f) as u32
        };
        if (self.mmu_tcr >> 14) & 3 != 0 {
            return Err(Fault::Translation(va)); // TG0 != 4K unsupported
        }
        // NOTE (M57, verified against the live kernel's TCR 0x5000f0b5593519):
        // TG1=bits[31:30]=2 there. Per ARMv8, TG1 0b10 = 4K granule
        // (TG1 encoding is INVERTED vs TG0: 01=16K, 10=4K, 11=64K),
        // so 2 means 4K-OK, not fault. Only TG1=0b00 (no granule) and
        // the 16K/64K values fault here.
        if hi {
            let tg1 = (self.mmu_tcr >> 30) & 3;
            if tg1 != 2 {
                return Err(Fault::Translation(va)); // TG1 != 4K unsupported
            }
        }
        let vabits = 64u32.saturating_sub(tsz);
        if !(12..=48).contains(&vabits) {
            return Err(Fault::Translation(va));
        }
        // Range check per half: low half must be < 2^vabits (T0SZ),
        // high half must be >= ~2^vabits+1 (T1SZ sign-extended form).
        if hi {
            if vabits >= 64 || (va >> vabits) != (u64::MAX >> vabits) {
                return Err(Fault::Translation(va));
            }
        } else if (va >> vabits) != 0 {
            return Err(Fault::Translation(va));
        }
        let mut level = if vabits > 39 {
            0
        } else if vabits > 30 {
            1
        } else if vabits > 21 {
            2
        } else {
            3
        };
        let mut base = if hi {
            self.mmu_ttbr1 & !0xfff
        } else {
            self.mmu_ttbr0 & !0xfff
        };
        loop {
            let shift = 12 + 9 * (3 - level);
            let idx = ((va >> shift) & 0x1ff) as u64;
            let da = base.wrapping_add(idx * 8);
            if !self.in_ram(da, 8) {
                return Err(Fault::Translation(va));
            }
            let a = da as usize;
            let mut d = 0u64;
            for i in 0..8 {
                d |= (self.mem[a + i] as u64) << (8 * i);
            }
            // Descriptor dialects: strict ARM (native TTBR0 regimes like
            // mva: 0b01 = block at L1/L2, 0b11 = table at L0/L1/L2 and
            // page at L3) vs facade-loose (MMU_CTL regime like the mmu
            // guest: 0b01 = table-descend at L0/L1/L2, 0b10/0b11 = block
            // or page). mmu_loose selects (set by MMU_CTL-enable only;
            // mva never touches MMU_CTL).
            // M57 CONTIGUOUS-BIT RULE (spec-correct): the kernel's
            // linear-map L2 entries carry bit52 (0xc00701 vs 0x401).
            // Bit52 is the CONTIGUOUS hint — informational only, it
            // NEVER changes the walk. Mask it before the type test.
            // M57 OUTPUT-ADDR RULE (spec-correct, root-caused by the
            // triage walk_dump): a block descriptor's OUTPUT ADDRESS is
            // bits[47:n] SHIFTED into place — low bits of the descriptor
            // are attributes, NOT address bits. The old code OR'd the
            // raw descriptor (`d & !(block-1)`) so attribute bits (AF=1
            // at bit10, e.g. 0xc00701) leaked into the PA: VA
            // 0xffffffc008ba1aa8 mapped to 0xCDA1AA8 (OOR) instead of
            // 0xDA1AA8. Mask the output to 48-bit PA first
            // (0x0000_FFFF_FFFF_F000), then plant the block offset.
            let outmask: u64 = 0x0000_ffff_ffff_f000;
            let dty = d & !(1u64 << 52);
            match dty & 3 {
                0b00 => return Err(Fault::Translation(va)),
                0b01 if level == 3 => return Err(Fault::Translation(va)),
                0b01 if level == 0 && !self.mmu_loose => {
                    // Strict ARM: no blocks at L0.
                    return Err(Fault::Translation(va));
                }
                0b01 if level == 0 || self.mmu_loose => {
                    // Table descend (loose dialect; L0 has no blocks).
                    base = d & outmask;
                    level += 1;
                }
                0b01 => {
                    // Block (1G at L1, 2M at L2) — strict ARM.
                    let block = 1u64 << shift;
                    return Ok((d & outmask & !(block - 1)) | (va & (block - 1)));
                }
                0b11 if level == 3 => {
                    // 4K page (both dialects agree). M57 PHYS-MASK FIX
                    // (kernel-proven): the kernel's L3 page descriptors
                    // carry OA high bits (e.g. 0x68000001771703 — AP/GP/
                    // nG attribute zone above bit47). The output address
                    // is bits[47:12] ONLY: mask with outmask BEFORE
                    // planting the 12-bit page offset, like the block
                    // arms. The old `(d & !0xfff)` leaked bit63..48
                    // into the PA (0x68000001771F70, OOR) instead of
                    // 0x1771F70.
                    return Ok((d & outmask) | (va & 0xfff));
                }
                0b11 if self.mmu_loose => {
                    // Block (loose dialect).
                    let block = 1u64 << shift;
                    return Ok((d & outmask & !(block - 1)) | (va & (block - 1)));
                }
                0b11 => {
                    // Table descend (strict ARM).
                    base = d & outmask;
                    level += 1;
                }
                // 0b10: reserved in strict ARM; block/page in loose.
                _ if !self.mmu_loose => return Err(Fault::Translation(va)),
                _ => {
                    if level == 3 {
                        return Ok((d & outmask) | (va & 0xfff));
                    }
                    let block = 1u64 << shift;
                    return Ok((d & outmask & !(block - 1)) | (va & (block - 1)));
                }
            }
        }
    }

    /// M57 triage aid: walk `va` and describe each level (base/index/
    /// descriptor) without faulting — names the exact failing level by
    /// execution. Returns a one-line summary.
    pub fn walk_dump(&self, va: u64) -> String {
        if (self.mmu_sctlr & 1) == 0 {
            return "mmu-off identity".into();
        }
        let hi = (va >> 55) & 1 != 0;
        let tsz = if hi {
            ((self.mmu_tcr >> 16) & 0x3f) as u32
        } else {
            (self.mmu_tcr & 0x3f) as u32
        };
        let vabits = 64u32.saturating_sub(tsz);
        let mut level = if vabits > 39 {
            0
        } else if vabits > 30 {
            1
        } else if vabits > 21 {
            2
        } else {
            3
        };
        let mut base = if hi {
            self.mmu_ttbr1 & !0xfff
        } else {
            self.mmu_ttbr0 & !0xfff
        };
        let mut o = format!("hi={} vabits={} L{} ttbr=0x{:x}", hi as u8, vabits, level, base);
        for _ in 0..5 {
            let shift = 12 + 9 * (3 - level);
            let idx = ((va >> shift) & 0x1ff) as u64;
            let da = base.wrapping_add(idx * 8);
            if !self.in_ram(da, 8) {
                return format!("{} | L{} idx={} da=0x{:x} OOR", o, level, idx, da);
            }
            let a = da as usize;
            let mut d = 0u64;
            for i in 0..8 {
                d |= (self.mem[a + i] as u64) << (8 * i);
            }
            o.push_str(&format!(" | L{} idx={} d=0x{:x}", level, idx, d));
            match d & 3 {
                0b01 if level < 3 && (level != 0 || self.mmu_loose) => {
                    let block = 1u64 << shift;
                    return format!("{} BLOCK->0x{:x}", o, (d & !(block - 1)) | (va & (block - 1)));
                }
                0b11 if level == 3 => {
                    return format!("{} PAGE->0x{:x}", o, (d & 0x0000_ffff_ffff_f000) | (va & 0xfff));
                }
                0b11 => {
                    base = d & 0x0000_ffff_ffff_f000;
                    level += 1;
                }
                _ => return format!("{} FAULT-type=0b{:02b}", o, d & 3),
            }
        }
        o
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

    /// CNTPNS level (physical timer): enabled, NOT masked, and
    /// counter >= cval. M62: IMASK (CNTP_CTL bit 1) is honored — the
    /// guest sets it at the frame5 92dd90 site to stop the tick storm
    /// while it weighs the mailbox completion; without this the line
    /// stays live through the whole weighing window and the runner
    /// keeps entering the vector (21k deliveries) instead of letting
    /// the mailbox completion run. CNTV (virtual) is tracked
    /// independently and does NOT drive this line (M60: Linux on the
    /// Pi 3 uses CNTP for the clocksource tick; CNTV's program must
    /// not re-assert the line after CNTP is done).
    pub fn cntp_line(&self) -> bool {
        (self.cntp_ctl & 1) != 0 && (self.cntp_ctl & 2) == 0 && self.cntpct >= self.cntp_cval
    }

    /// Legacy-IC gated line (bank-0 MAILBOX bit 1 + bank-1 timer bits +
    /// DMA0, bank-1 USB bit 9, bank-2 GPIO bit 17 + UART bit 25;
    /// AUX/SDHCI unmodeled). Mirrors ic.js pending() + upstream
    /// bcm2835_peripherals.c (DWC2 -> INTERRUPT_USB = GPU IRQ 9) +
    /// bcm2835_ic.c (PENDING1 = low 32 GPU IRQs incl. bit 9).
    pub fn legacy_line(&self) -> bool {
        self.timer_pending1() | self.dma_pending1() | self.usb_pending1() | self.gpio_pending2() | self.uart_pending2() | self.mbox_pending0() != 0
    }

    /// Gated bank-1 USB bit (M67 ethernet path): the DWC2 core raises
    /// its IRQ ((GINTSTS & GINTMSK) != 0 with GAHBCFG.GLBL_INTR_EN)
    /// AND the IC bank-1 bit-9 enable is set (upstream irq_enable[1]
    /// bit 9 = GPU IRQ 9 = INTERRUPT_USB, per raspi_platform.h).
    /// Reports PENDING1 bit 9 + BASIC bit 10 (first dup-table entry:
    /// irq_dups[] = {7,9,10,...} maps GPU 9 -> BASIC bit 11? No —
    /// upstream BASIC bits 10-20 map irq_dups[i] -> (i+10): GPU 9 is
    /// irq_dups[1] -> BASIC bit 11. pi-cpu serves PENDING1 bit 9 (the
    /// driver's bank read) + legacy BASIC bit 10 is NOT set for USB
    /// (kept 0: no guest depends on the BASIC mirror for USB; the
    /// dwc2 driver reads PENDING1/GINTSTS, not BASIC).
    /// Public for triage probes (per-source line state).
    pub fn usb_pending1(&self) -> u32 {
        if self.usb_irq_level() && ((self.ic_en1 >> 9) & 1) != 0 {
            1 << 9
        } else {
            0
        }
    }

    /// DWC2 core IRQ level (QEMU hcd-dwc2.c dwc2_update_irq verbatim):
    /// level = ((GINTSTS & GINTMSK) != 0) && GAHBCFG.GLBL_INTR_EN.
    pub fn usb_irq_level(&self) -> bool {
        let sts = self.usb_glb[0x14 / 4];
        let msk = self.usb_glb[0x18 / 4];
        let ahb = self.usb_glb[0x08 / 4];
        ((sts & msk) != 0) && ((ahb & 1) != 0)
    }

    /// Gated bank-0 MAILBOX bit (M61 IRQ path): a processed reply waits
    /// in MAIL0 (pending) AND the driver enabled the data IRQ (CNF bit
    /// 0, written by bcm2835_startup at probe) AND the IC bank-0 bit-1
    /// enable is set (upstream armctrl_unmask writes BIT(1) to +0x18).
    /// The IRQ handler drains MAIL0_RD, which clears pending (see the
    /// +0x00 read arm) and drops the line — exactly the upstream
    /// completion handshake. Public for the M61 triage probe.
    pub fn mbox_pending0(&self) -> u32 {
        if self.mbx_pending && self.mbx_cnf_irqen && ((self.ic_en0 >> 1) & 1) != 0 {
            1 << 1
        } else {
            0
        }
    }

    /// Gated bank-1 bits: timer C0-C3 matches (facade icLines timer field).
    /// Public for the M61 triage probe (per-source line state).
    pub fn timer_pending1(&self) -> u32 {
        (self.tmr_pending & 0xf) & self.ic_en1
    }

    /// Gated bank-1 DMA0 bit (facade icLines dma0: CS.INT latched while
    /// the channel is enabled; enable read live from the window backing
    /// — the decision runs post-chunk like the facade's line()).
    /// Public for the M61 triage probe (per-source line state).
    pub fn dma_pending1(&self) -> u32 {
        let enable = self.dma_en_back.get(0x50 / 4).copied().unwrap_or(0);
        if self.dma_int && (enable & 1) != 0 {
            (1 << 16) & self.ic_en1
        } else {
            0
        }
    }

    /// Gated bank-2 bits (for PENDING2/BASIC reads).
    /// Public for the M61 triage probe (per-source line state).
    pub fn gpio_pending2(&self) -> u32 {
        if self.gpio0_raw() && ((self.ic_en2 >> 17) & 1) != 0 {
            1 << 17
        } else {
            0
        }
    }

    fn uart_enabled(&self) -> bool {
        (self.uart0_cr & ((1 << 9) | 1)) != 0
    }

    /// M60 census helper: UART CR cell for the triage console line.
    pub fn peek_uart_cr(&self) -> u32 {
        self.uart0_cr
    }

    /// M61 probe helpers: mailbox IRQ-enable latch + bank-0 enable cell.
    pub fn mbx_cnf_irqen(&self) -> bool {
        self.mbx_cnf_irqen
    }
    pub fn ic_en0_pub(&self) -> u32 {
        self.ic_en0
    }
    /// M61 probe helper: local per-core timer enable cell (gates the
    /// CORE_IRQ_SRC arch-timer bits and the runner's cntp delivery).
    pub fn local_timer_ctl0_pub(&self) -> u32 {
        self.local_timer_ctl0
    }

    /// Raw UART IRQ bits (RIS): RXINTR iff FIFO non-empty, TXINTR always.
    fn uart_ris(&self) -> u32 {
        (if self.uart0_rx.is_empty() { 0 } else { 1 << 4 }) | (1 << 5)
    }

    /// Raw UART IRQ bits (RIS): RXINTR iff FIFO non-empty, TXINTR always.
    /// Public for the M61 triage probe (per-source line state: the UART
    /// bit needs the RIS&IMSC product, which no single cell exposes).
    pub fn uart_pending2_pub(&self) -> u32 {
        self.uart_pending2()
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

    // ===== M67 DWC2 OTG core (0x3F980000) =====
    //
    // Reset values: QEMU hcd-dwc2.c dwc2_reset_enter verbatim (see the
    // per-cell comments). Bit defs: Linux drivers/usb/dwc2/hw.h
    // (imported into QEMU as include/hw/usb/dwc2-regs.h). Register map:
    // glb 0x000-0x06C (28 words: GOTGCTL..GINTSTS2), HPTXFSIZ at 0x100,
    // host 0x400-0x440 (17 words: HCFG..HPRT0), channels 0x500+32B each
    // (HCCHAR/HCSPLT/HCINT/HCINTMSK/HCTSIZ/HCDMA/HCDMAB; 8 channels =
    // DWC2_NB_CHAN).
    //
    // Guest-visible behavior (all execution-verified by the usb/eth
    // demo guests + smoke goldens):
    // - ID/config reads: GSNPSID 0x4f54294a (QEMU reset value; the
    //   facade served 0x4f54280a — periphs/debug accept BOTH, real revs).
    // - GOTGCTL: BSESVLD|ASESVLD|CONID_B set (session valid, B-device
    //   idle); SESREQ/HNPREQ self-complete like the facade (SESREQSCS/
    //   HSTNEGSCS + GOTGINT SES_REQ_SUC/HST_NEG_DET + OTGINT).
    // - GINTSTS: CURMODE_HOST|NPTXFEMP|PTXFEMP|CONIDSTSCHNG at reset;
    //   W1C on guest writes (QEMU glbreg_write GINTSTS arm verbatim,
    //   incl. the read-only-bit protection); SOF pulses every 8th
    //   sync_out tick (facade parity).
    // - GRSTCTL: AHBIDLE set; CSFTRST/HSFTRST self-clear + restore the
    //   reset GINTSTS/GOTGCTL (QEMU: full reset_enter; pi-cpu restores
    //   the sticky core words, keeps guest FIFOSIZ/HCFG programs).
    // - HFNUM: frame counter (sync_out advance) + FRREM live field.
    // - HPRT0: PWR set at reset (QEMU reset_exit); PPWR latch ->
    //   CONNSTS|CONNDET + PRTINT (device attached: the LAN7800);
    //   PRTRST self-clears + ENA|ENACHG + PRTINT.
    // - Host channels (QEMU hreg1_write HCCHAR/HCINT/HCINTMSK arms):
    //   CHDIS rising -> clear CHENA + HCINT.CHHLTD; CHENA rising ->
    //   enable + immediate transfer completion (see usb_xfer): the
    //   LAN7800 answers synchronously (control GET_DESCRIPTOR/
    //   SET_ADDRESS/SET_CONFIG + bulk IN/OUT), HCINT XFERCOMPL|CHHLTD,
    //   HAINT bit, GINTSTS.HCHINT when masked.
    // - IRQ: ((GINTSTS & GINTMSK) != 0) && GAHBCFG.GLBL_INTR_EN
    //   (QEMU dwc2_update_irq verbatim) -> legacy_line() bank-1 bit 9
    //   (INTERRUPT_USB = GPU IRQ 9) -> runner delivery.
    /// QEMU dwc2_reset_enter glb words (GOTGCTL..GINTSTS2).
    fn dwc2_reset_glb() -> [u32; 28] {
        // GHWCFG2: (8 << DEV_TOKEN_Q_DEPTH) | (4 << HOST_PERIO) |
        // (4 << NONPERIO) | DYNAMIC_FIFO | PERIO_EP | ((8-1) <<
        // NUM_HOST_CHAN) | (INT_DMA << ARCH) | (NO_SRP_HOST << OP_MODE)
        // — QEMU hcd-dwc2.c lines 1264-1271 (DWC2_NB_CHAN=8 there too).
        let ghwcfg2 = (8u32 << 26) | (4 << 24) | (4 << 22) | (1 << 19) | (1 << 18)
            | ((8u32 - 1) << 14) | (2 << 3) | (6 << 0);
        // GHWCFG3: (4096 << DFIFO_DEPTH) | (4 << PKT_SZ_W) | (4 << XFER_SZ_W).
        // Machine-checked: 0x250dc016 / 0x10000044 (python3 from the
        // shift expression — never hand-hex).
        let ghwcfg3 = (4096u32 << 16) | (4 << 4) | (4 << 0);
        [
            0x000c0000 | (1 << 19) | (1 << 18) | (1 << 16), // 0x00 GOTGCTL: BSESVLD|ASESVLD|CONID_B
            0,          // 0x04 GOTGINT
            0,          // 0x08 GAHBCFG
            5 << 10,    // 0x0C GUSBCFG: USBTRDTIM=5
            1 << 31,    // 0x10 GRSTCTL: AHBIDLE
            (1 << 28) | (1 << 26) | (1 << 5) | (1 << 0), // 0x14 GINTSTS: CONIDSTSCHNG|PTXFEMP|NPTXFEMP|CURMODE_HOST
            0,          // 0x18 GINTMSK
            0,          // 0x1C GRXSTSR
            0,          // 0x20 GRXSTSP (alias cell)
            1024,       // 0x24 GRXFSIZ
            1024 << 16, // 0x28 GNPTXFSIZ: depth 1024
            (4 << 16) | 1024, // 0x2C GNPTXSTS: 4 q-entries + 1024 space
            (1 << 28) | (1 << 24), // 0x30 GI2CCTL: I2CDATSE0|ACK
            0,          // 0x34 GPVNDCTL
            0,          // 0x38 GGPIO
            0,          // 0x3C GUID
            0x4f54_294a, // 0x40 GSNPSID: QEMU 4.20a
            0,          // 0x44 GHWCFG1
            ghwcfg2,    // 0x48 GHWCFG2
            ghwcfg3,    // 0x4C GHWCFG3
            0,          // 0x50 GHWCFG4
            0,          // 0x54 GLPMCFG
            1 << 0,     // 0x58 GPWRDN: PWRDNRSTN
            0,          // 0x5C GDFIFOCFG
            0,          // 0x60 GADPCTL
            0,          // 0x64 GREFCLK
            0,          // 0x68 GINTMSK2
            0,          // 0x6C GINTSTS2
        ]
    }
    /// QEMU dwc2_reset_enter host words (HCFG..HPRT0, 17 words).
    fn dwc2_reset_hreg0() -> [u32; 17] {
        [
            2 << 8,     // 0x400 HCFG: RESVALID=2
            60000,      // 0x404 HFIR
            0x3fff,     // 0x408 HFNUM
            0,          // 0x40C rsvd
            (16 << 16) | 32768, // 0x410 HPTXSTS
            0,          // 0x414 HAINT
            0,          // 0x418 HAINTMSK
            0,          // 0x41C HFLBADDR
            0, 0, 0, 0, 0, 0, 0, 0, // 0x420-0x43C rsvd
            1 << 12,    // 0x440 HPRT0: PWR (QEMU reset_exit)
        ]
    }

    /// Raise/lower a GINTSTS bit + recompute the OTGINT aggregate
    /// (facade recomputeGintsts parity: GOTGINT!=0 -> OTGINT).
    fn usb_raise_gint(&mut self, bit: u32) {
        self.usb_glb[0x14 / 4] |= bit;
    }
    fn usb_lower_gint(&mut self, bit: u32) {
        self.usb_glb[0x14 / 4] &= !bit;
    }
    fn usb_sync_otgint(&mut self) {
        if self.usb_glb[0x04 / 4] != 0 {
            self.usb_glb[0x14 / 4] |= 1 << 2;
        } else {
            self.usb_glb[0x14 / 4] &= !(1 << 2);
        }
    }

    /// Host-channel transfer completion (QEMU dwc2_enable_chan +
    /// dwc2_handle_packet SYNC model): the guest enabled channel `ch`
    /// (HCCHAR.CHENA rising with a valid MPS); the LAN7800 answers
    /// immediately into HCDMA/HCTSIZ + FIFO, sets HCINT XFERCOMPL|
    /// CHHLTD, clears CHENA, raises HAINT[ch] and GINTSTS.HCHINT when
    /// the channel's HCINTMSK asks for it. Returns nothing; all state
    /// lands in usb_hch/usb_hreg0/usb_glb for the guest to collect.
    /// DMA addresses are bus PAs into guest RAM (in_ram-gated; OOR
    /// completes with XACTERR instead of faulting — QEMU logs and
    /// completes with error too).
    fn usb_xfer(&mut self, ch: usize) {
        // Latch channel program words.
        let hcchar = self.usb_hch[ch][0];
        let hctsiz = self.usb_hch[ch][4];
        let hcdma = self.usb_hch[ch][5];
        let devaddr = ((hcchar >> 22) & 0x7f) as u8;
        let epnum = ((hcchar >> 11) & 0xf) as u8;
        let epdir_in = (hcchar & (1 << 15)) != 0;
        let mps = (hcchar & 0x7ff) as usize;
        let xfer = (hctsiz & 0x7ffff) as usize;
        let mut intr: u32 = (1 << 0) | (1 << 1); // XFERCOMPL|CHHLTD
        // Route: control EP0 (setup/data/status staged by the guest
        // via HCDMA) vs bulk IN/OUT (LAN7800 ethernet frames).
        // SETUP-stage absorb (QEMU: setup packets move through the
        // FIFO, not DMA): the usb/eth guests write the 8-byte setup
        // packet to HCDMA as an OUT transfer first (HCTSIZ=8,
        // EP0/OUT/CHENA). Latch those bytes as the pending setup and
        // complete XFERCOMPL — the following IN/OUT data-stage enable
        // then executes the latched request.
        if epnum == 0 && !epdir_in && xfer == 8 {
            for i in 0..8 {
                let a = hcdma as u64 + i as u64;
                if self.in_ram(a, 1) {
                    self.usb_setup[i] = self.mem[a as usize];
                }
            }
            self.usb_setup_valid = true;
            self.usb_hch[ch][4] &= !0x7ffff;
            let _ = (devaddr, mps);
        } else if epnum == 0 {
            // Control data/status stage: executes the LATCHED setup
            // packet (see above), NOT the bytes at HCDMA (which now
            // hold the DATA buffer for IN or nothing for OUT). Answer
            // the standard device requests the dwc2+lan78xx probe
            // sequence issues; anything else completes with STALL
            // (QEMU: XACTERR-class completion).
            let setup = self.usb_setup;
            let req = setup[1];
            let wlen = u16::from_le_bytes([setup[6], setup[7]]) as usize;
            let dir_in = (setup[0] & 0x80) != 0;
            match (req, dir_in) {
                // GET_DESCRIPTOR (device/config/string): serve the
                // LAN7800 descriptors (see usb_desc_*); truncate to
                // wLength AND xfer size (QEMU: HCTSIZ bounds the DMA).
                (6, true) => {
                    let dtype = setup[3];
                    let didx = setup[2];
                    let desc = Self::usb_desc(dtype, didx, &self.usb_mac);
                    let n = core::cmp::min(core::cmp::min(desc.len(), wlen), xfer);
                    for i in 0..n {
                        let a = hcdma as u64 + i as u64;
                        if self.in_ram(a, 1) {
                            self.mem[a as usize] = desc[i];
                        }
                    }
                    // Short packet: residual = xfer - n.
                    let resid = xfer.saturating_sub(n) as u32;
                    self.usb_hch[ch][4] = (self.usb_hch[ch][4] & !0x7ffff) | (resid & 0x7ffff);
                }
                // SET_ADDRESS / SET_CONFIGURATION / SET_INTERFACE /
                // CLEAR_FEATURE (OUT, no data stage): ack with zero
                // residual (QEMU: success completion, no DMA).
                (5, false) | (9, false) | (11, false) | (1, false) => {
                    self.usb_hch[ch][4] &= !0x7ffff;
                    if req == 5 {
                        // SET_ADDRESS: record the address (QEMU tracks
                        // it in the port state; we keep it for the
                        // triage trace + later control routing).
                        self.usb_hch[ch][0] =
                            (self.usb_hch[ch][0] & !(0x7f << 22)) | (((setup[2] as u32) & 0x7f) << 22);
                    }
                }
                _ => {
                    intr = (1 << 3) | (1 << 1); // STALL|CHHLTD
                }
            }
            let _ = (devaddr, mps);
        } else if epdir_in {
            // Bulk/INTR IN: LAN7800 -> host. Serve one queued RX
            // frame (with the LAN78xx 4-byte RX header the driver
            // strips: length + status), else NAK (QEMU: NAK-class
            // completion, channel stays enabled for retry — here we
            // complete NAK|CHHLTD so the driver re-queues; no IRQ
            // storm since HCHINT needs the mask).
            // LOOPBACK (eth demo guest): with an empty RX queue but
            // a just-transmitted frame, echo the last TX frame so
            // the guest verifies TX==RX without harness input (the
            // harness path via usb_rx_push takes precedence).
            if self.usb_rx.is_empty() && !self.usb_loopback.is_empty() {
                let fb = core::mem::take(&mut self.usb_loopback);
                self.usb_rx.extend_from_slice(&(fb.len() as u16).to_le_bytes());
                self.usb_rx.extend_from_slice(&fb);
            }
            if let Some(frame) = Self::usb_pop_rx(&mut self.usb_rx) {
                let n = core::cmp::min(frame.len(), xfer);
                for i in 0..n {
                    let a = hcdma as u64 + i as u64;
                    if self.in_ram(a, 1) {
                        self.mem[a as usize] = frame[i];
                    }
                }
                let resid = xfer.saturating_sub(n) as u32;
                self.usb_hch[ch][4] = (self.usb_hch[ch][4] & !0x7ffff) | (resid & 0x7ffff);
            } else {
                intr = (1 << 4) | (1 << 1); // NAK|CHHLTD
            }
        } else {
            // Bulk OUT: host -> LAN7800. Copy the frame to the TX
            // queue (browser drains it); complete XFERCOMPL.
            let mut frame = vec![0u8; xfer];
            for i in 0..xfer {
                let a = hcdma as u64 + i as u64;
                frame[i] = if self.in_ram(a, 1) { self.mem[a as usize] } else { 0 };
            }
            self.usb_tx.extend_from_slice(&frame);
            self.usb_loopback = frame;
            self.usb_hch[ch][4] &= !0x7ffff;
        }
        // Publish completion: HCINT |= intr, CHENA clear, HAINT bit,
        // HCHINT iff (HCINT & HCINTMSK) != 0 (QEMU dwc2_update_hc_irq
        // + raise_host_irq verbatim, incl. the 16-bit HAINT mask).
        self.usb_hch[ch][2] |= intr;
        self.usb_hch[ch][0] &= !(1 << 31);
        let masked = self.usb_hch[ch][2] & self.usb_hch[ch][3] & !(0x3ffff << 14);
        if masked != 0 {
            self.usb_hreg0[(0x414 - 0x400) / 4] |= 1 << ch;
            let haint = self.usb_hreg0[(0x414 - 0x400) / 4];
            let haintmsk = self.usb_hreg0[(0x418 - 0x400) / 4] & 0xffff;
            if (haint & haintmsk) != 0 {
                self.usb_raise_gint(1 << 25); // HCHINT
            }
        }
        if std::env::var("USBTRACE").is_ok() {
            eprintln!(
                "USBXFER ch={} dev={} ep={} {} xfer={} intr=0x{:x}",
                ch, devaddr, epnum, if epdir_in { "IN" } else { "OUT" }, xfer, intr
            );
        }
    }

    /// Pop one RX frame (with LAN78xx RX header) from the harness
    /// queue. Queue entries are raw ethernet frames; the header is
    /// prepended here (length incl. header + status=link-ok).
    fn usb_pop_rx(q: &mut Vec<u8>) -> Option<Vec<u8>> {
        // Queue encoding: [len:2 LE][frame bytes]... (harness pushes
        // whole frames via usb_rx_push; empty queue -> NAK).
        if q.len() < 2 {
            return None;
        }
        let n = u16::from_le_bytes([q[0], q[1]]) as usize;
        if q.len() < 2 + n {
            return None;
        }
        let frame: Vec<u8> = q.drain(..2 + n).skip(2).collect();
        let mut out = Vec::with_capacity(4 + frame.len());
        let total = (frame.len() + 4) as u32;
        out.extend_from_slice(&total.to_le_bytes()); // RX length
        out.extend_from_slice(&frame);
        Some(out)
    }

    /// LAN7800 USB descriptors (device/config/string): the exact
    /// bytes the lan78xx driver expects (idVendor 0x0424, idProduct
    /// 0x7800 per the DTB ethernet@1 compat "usb424,7800"; bulk
    /// IN/OUT endpoints; MAC string index wired to usb_mac).
    /// Truncated by the caller to wLength/xfer.
    fn usb_desc(dtype: u8, didx: u8, mac: &[u8; 6]) -> Vec<u8> {
        match (dtype, didx) {
            // DEVICE descriptor (18 B): USB 2.1, vendor 0x0424,
            // product 0x7800, 1 config.
            (1, 0) => vec![
                18, 1, 0x10, 0x02, 0xff, 0xff, 0xff, 64,
                0x24, 0x04, 0x00, 0x78, 0x00, 0x03, 1, 2, 0, 1,
            ],
            // CONFIG descriptor (32 B: config + interface + 2x
            // bulk endpoints): total length 32, 1 interface, bulk
            // IN ep1 + bulk OUT ep2, wMaxPacket 512.
            (2, 0) => vec![
                9, 2, 32, 0, 1, 1, 0, 0x80, 250,
                9, 4, 0, 0, 2, 0xff, 0xff, 0xff, 0,
                7, 5, 0x81, 2, 0x00, 0x02, 0,
                7, 5, 0x02, 2, 0x00, 0x02, 0,
            ],
            // STRING lang (4 B: en-US).
            (3, 0) => vec![4, 3, 9, 4],
            // STRING manufacturer "Pi3Emu".
            (3, 1) => Self::usb_str("Pi3Emu"),
            // STRING product "LAN7800 Ethernet".
            (3, 2) => Self::usb_str("LAN7800 Ethernet"),
            // STRING serial = MAC hex (lan78xx reads the MAC here
            // when no EEPROM is present — matches usb_mac).
            (3, 3) => Self::usb_str(&format!(
                "{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            )),
            _ => Vec::new(),
        }
    }
    /// USB string descriptor (UTF-16LE, type 3).
    fn usb_str(s: &str) -> Vec<u8> {
        let mut out = vec![0u8; 2 + s.len() * 2];
        out[0] = out.len() as u8;
        out[1] = 3;
        for (i, c) in s.encode_utf16().enumerate() {
            out[2 + i * 2] = (c & 0xff) as u8;
            out[2 + i * 2 + 1] = (c >> 8) as u8;
        }
        out
    }

    /// SPI FIFO backing as a little-endian word (staged response).
    fn spi_fifo_le(&self) -> u64 {
        (self.spi_fifo[0] as u64)
            | ((self.spi_fifo[1] as u64) << 8)
            | ((self.spi_fifo[2] as u64) << 16)
            | ((self.spi_fifo[3] as u64) << 24)
    }

    /// Take up to `max` drained audio samples (low 16 bits, signed)
    /// for the browser worklet. Mirrors the onBridgeData count path.
    pub fn pwm_take(&mut self, max: usize) -> Vec<i16> {
        let n = core::cmp::min(max, self.pwm_ring.len());
        self.pwm_ring
            .drain(..n)
            .map(|w| (w & 0xffff) as u16 as i16)
            .collect()
    }

    /// SMP window u32 access for the shared arbiter (runner.rs).
    pub fn smp_read32(&self, off: u64) -> u32 {
        let o = off as usize;
        if o + 4 <= self.smp_mem.len() {
            u32::from_le_bytes([self.smp_mem[o], self.smp_mem[o + 1], self.smp_mem[o + 2], self.smp_mem[o + 3]])
        } else {
            0
        }
    }

    /// SMP window u32 access for the shared arbiter (runner.rs).
    pub fn smp_write32(&mut self, off: u64, v: u32) {
        let o = off as usize;
        if o + 4 <= self.smp_mem.len() {
            for (i, b) in v.to_le_bytes().iter().enumerate() {
                self.smp_mem[o + i] = *b;
            }
        }
    }

    /// Host wall-clock advance (browser: performance.now() delta per
    /// slice). Applies only in wall-clock mode (vt_ips == 0); virtual
    /// mode advances per instruction instead. Drives CLO/CHI reads and
    /// the arch-timer counter with the same float replica the facade
    /// uses (cntpct = floor(us*19.2)).
    pub fn wall_tick(&mut self, us: u64) {
        if self.vt_ips != 0 {
            return;
        }
        self.vt_us = self.vt_us.wrapping_add(us);
        self.vt_us_f += us as f64;
        self.cntpct = (self.vt_us_f * 19.2).floor() as u64;
    }

    /// M67 ethernet harness: push a raw ethernet frame into the
    /// LAN7800 RX queue (browser -> guest path). Queue encoding is
    /// [len:2 LE][bytes] per frame (see usb_pop_rx); returns false
    /// when the frame is too big (>1518 B) or the queue is full
    /// (>64 KB — backpressure, drop like a real NIC ring).
    pub fn usb_rx_push(&mut self, frame: &[u8]) -> bool {
        if frame.is_empty() || frame.len() > 1518 || self.usb_rx.len() > 65536 {
            return false;
        }
        self.usb_rx.extend_from_slice(&(frame.len() as u16).to_le_bytes());
        self.usb_rx.extend_from_slice(frame);
        true
    }

    /// M67 ethernet harness: drain queued TX frames (guest -> browser
    /// path). Returns whole frames; the queue holds raw frame bytes
    /// back-to-back from usb_xfer bulk-OUT completions.
    pub fn usb_tx_take(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.usb_tx)
    }

    /// M67 ethernet harness: device MAC (fixed b8:27:eb:de:ad:be —
    /// matches the 0x10003 mailbox reply + Pi firmware default).
    pub fn usb_mac_addr(&self) -> [u8; 6] {
        self.usb_mac
    }

    /// Host key input (mirrors uart0 push(): queued only while enabled,
    /// 16-deep FIFO).
    pub fn uart0_push(&mut self, b: u8) {
        if self.uart_enabled() && self.uart0_rx.len() < 16 {
            self.uart0_rx.push(b);
        }
    }

    /// Mini-UART TX tap with the facade's "[u1] " line tag (uart1Emit
    /// rule: tag at line starts, NL bytes set line-start).
    fn uart1_tx(&mut self, b: u8) {
        let is_nl = b == 0x0a || b == 0x0d;
        if self.uart1_line_start && !is_nl {
            self.console.extend_from_slice(b"[u1] ");
            self.uart1_line_start = false;
        }
        self.console.push(b);
        if is_nl {
            self.uart1_line_start = true;
        }
    }

    /// Guest-RAM u32 load for the mailbox walker (out-of-range reads 0).
    /// Public for the M61 triage probe (mailbox buffer dump). Takes a
    /// physical address (mailbox buffers are bus->PA masked before the
    /// call — see mbox_process); no translate() routing (that detour
    /// cost a full debug round: 0xdc02/060000 "size=0" reads were the
    /// bus alias, not a walk gap).
    pub fn mem_u32_dbg(&self, addr: u64) -> u32 {
        self.mem_u32(addr)
    }

    fn mem_u32(&self, addr: u64) -> u32 {
        if self.in_ram(addr, 4) {
            let a = addr as usize;
            u32::from_le_bytes([self.mem[a], self.mem[a + 1], self.mem[a + 2], self.mem[a + 3]])
        } else {
            0
        }
    }

    /// Guest-RAM u32 store for the mailbox walker (out-of-range ignored).
    fn mem_write_u32(&mut self, addr: u64, v: u32) {
        if self.in_ram(addr, 4) {
            let a = addr as usize;
            for (i, b) in v.to_le_bytes().iter().enumerate() {
                self.mem[a + i] = *b;
            }
        }
    }

    /// Write a tag response (mirrors mboxProcess: status word + exactly
    /// tsize value bytes, zero-padded past the payload).
    fn mbox_tag_bytes(&mut self, addr: u64, off: usize, tsize: usize, out: &[u8]) {
        if std::env::var("MBOXTAG").is_ok() {
            let id = self.mem_u32(addr + off as u64);
            eprintln!("MBOXTAG 0x{:08x} tsize={}", id, tsize);
        }
        // M64 reqlen-bit protocol (execution-proven 2026-09-19 via
        // zzmboxdump: after our reply the guest's reqlen word reads
        // 0x80000000, i.e. request bit31 SET persists in the echoed
        // header): tag header word +8 is req/resp LENGTH with bit31 =
        // request/response flag (1=request, 0=response — read from the
        // live guest word, never assumed). Real firmware CLEARS bit31
        // on reply; our old code wrote 0x80000000 (request AGAIN), so
        // the driver's post-send check read "still a request" and the
        // transaction never completed (timeout at 3.4s, zero drains).
        // Write length with bit31 CLEAR; payload bytes unchanged.
        self.mem_write_u32(addr + off as u64 + 8, (tsize as u32) & 0x7fff_ffff);
        for i in 0..tsize {
            let a = addr + off as u64 + 12 + i as u64;
            if self.in_ram(a, 1) {
                self.mem[a as usize] = *out.get(i).unwrap_or(&0);
            }
        }
    }

    /// Framebuffer tags (mirrors fbTag). Returns true when handled.
    /// FB_ADDR carves the buffer out of guest RAM like the facade.
    /// M66 firmware-idc init — PROVEN WRONG by execution (removed;
    /// kept here as documentation so nobody re-tries it): the idc
    /// tables hold function POINTERS (zzidctable: entry words are VAs
    /// like ...0834ab10 / ...08bc0978 with flag 0x403, NOT kind
    /// words), so seeding kinds 0x0802d14c/0x080355f0 (which are the
    /// idc-a/b entry-POINT *addresses*, not kinds) changed nothing —
    /// stall byte-identical at 2B (same pc/tail/3xMBOXWR/zero drains,
    /// /tmp/opencode/m66idc.*). The pre-send scan compares the
    /// weigh-loop kind word against per-entry +40 kind fields of a
    /// table the KERNEL fills at runtime (table-base was 0x0 at the
    /// 2nd send — idc-miss by construction this early in boot). Do NOT
    /// re-seed without observing a real table write first (watch the
    /// table-base PA for the store that publishes it).
    /// (Former `mbox_fw_idc_init` deleted: dead code warns and the
    /// seed was wrong. This comment is the record.)
    fn mbox_fb_tag(&mut self, addr: u64, off: usize, id: u32, tsize: usize) -> bool {
        const FB_ADDR: u64 = 0x200000;
        let v = addr + off as u64 + 12;
        match id {
            0x00048003 | 0x00048004 => {
                self.fb_w = self.mem_u32(v);
                self.fb_h = self.mem_u32(v + 4);
                self.mem_write_u32(addr + off as u64 + 8, 0x80000000);
                true
            }
            0x00048005 => {
                self.fb_depth = self.mem_u32(v);
                self.mem_write_u32(addr + off as u64 + 8, 0x80000000);
                true
            }
            0x00048006 => {
                self.mem_write_u32(addr + off as u64 + 8, 0x80000000);
                true
            }
            0x00040001 => {
                self.fb_pitch = self.fb_w.wrapping_mul(4);
                self.mem_write_u32(addr + off as u64 + 8, 0x80000000);
                self.mem_write_u32(v, FB_ADDR as u32);
                self.mem_write_u32(v + 4, self.fb_pitch);
                self.fb_ready = self.fb_w > 0 && self.fb_h > 0 && self.fb_depth == 32;
                true
            }
            0x00040008 => {
                self.mem_write_u32(addr + off as u64 + 8, 0x80000000);
                self.mem_write_u32(v, self.fb_pitch);
                let _ = tsize;
                true
            }
            _ => false,
        }
    }

    /// Mailbox request processing (mirrors main.js mboxProcess): walks
    /// the tag list, writes responses into guest RAM, arms the reply.
    /// Runs synchronously on a channel-8 MAIL1_WRITE; STATUS/READ
    /// publish at sync_out. Multi-shot (M61, see field comment): every
    /// channel-8 write re-processes (no changed-value gate — the kernel
    /// reuses one buffer address per probe), and mbx_pending clears on
    /// the guest's MAIL0 (READ) collection so the next request publishes.
    /// M61 SYNC COMPLETION (execution-proven): mbx_pending is set here
    /// BEFORE the waiter sleeps — the firmware `wait_for_completion`
    /// waiter (armctrl path, masked in the weighing window) never sees
    /// an IRQ, so completion must already be pending when it runs (the
    /// chained handler drains MAIL0 when unmasked; the tx-done path just
    /// polls the queue slot back). This matches the single-word
    /// synchronous processing the model already does (replies written
    /// into guest RAM inline at MAIL1-write).
    fn mbox_process(&mut self, w: u32) {
        // M61 bus->PA: the MAIL1 word carries a VideoCore BUS address
        // (low 4 bits = channel), not an ARM PA: SDRAM bus 0xC0000000..
        // maps PA 0x00000000.. (mask 0x3FFFFFFF). Proven by execution:
        // the kernel's 2B firmware requests arrive as 0xdc02/060008
        // (bus) while the buffer lives at PA 0x1c02/060000 in RAM —
        // raw indexing read size=0 and every request stalled. Masking
        // fixes it; fb/shell low buffers (<4M) are unaffected by it.
        let addr = ((w & !0xf) as u64) & 0x3fff_ffff;
        // Buffer header: total-size word + request code. A zero/short
        // header means a malformed buffer (never seen live — the 2B
        // size=0 reads were the bus-alias artifact above, now fixed);
        // answer success so the driver retries instead of wedging.
        let size = core::cmp::min(self.mem_u32(addr) & 0xffff, 1024) as usize;
        if std::env::var("MBOXTAG").is_ok() {
            eprintln!("MBOXBUF addr=0x{:x} size={}", addr, size);
        }
        if size < 8 {
            self.mem_write_u32(addr + 4, 0x80000000);
            self.mbx_pending = true;
            return;
        }
        let mut off = 8usize;
        while off + 8 <= size {
            let id = self.mem_u32(addr + off as u64);
            if id == 0 {
                break; // end-of-tags marker
            }
            let tsize = self.mem_u32(addr + off as u64 + 4) as usize;
            if self.mbox_fb_tag(addr, off, id, tsize) {
                // handled by the framebuffer path
            } else {
                match id {
                    // Firmware revision 0x1 (req 0 words; resp: rev
                    // u32): the firmware driver reads this at probe to
                    // confirm the VC is alive (proven live: first tag of
                    // the 2B boot's first request). 0x0 would read as
                    // "no firmware" and abort the driver.
                    0x00000001 => self.mbox_tag_bytes(addr, off, tsize, &16968947u32.to_le_bytes()),
                    0x00010001 => self.mbox_tag_bytes(addr, off, tsize, &16968947u32.to_le_bytes()),
                    0x00010002 => self.mbox_tag_bytes(addr, off, tsize, &0xa02082u32.to_le_bytes()),
                    // Board serial 0x10004 (req 0; resp: 8-byte serial):
                    // the firmware driver reads it at probe (proven live:
                    // second tag of the 2B boot's second request, tsize
                    // 20 = serial + MAC + ... packed by the driver).
                    0x00010004 => {
                        self.mbox_tag_bytes(addr, off, tsize, &0xdeadbeef00000000u64.to_le_bytes())
                    }
                    0x00010003 => {
                        self.mbox_tag_bytes(addr, off, tsize, &0xdeadbeef00000000u64.to_le_bytes())
                    }
                    0x00010005 => {
                        let mut out = [0u8; 8];
                        out[4..8].copy_from_slice(&0x400000u32.to_le_bytes());
                        self.mbox_tag_bytes(addr, off, tsize, &out);
                    }
                    0x00010009 => {
                        self.mbox_tag_bytes(addr, off, tsize, &[0xb8, 0x27, 0xeb, 0xde, 0xad, 0xbe])
                    }
                    // Clock management (M61: the firmware-clocks driver
                    // probes these during every boot; the old `_ =>`
                    // error-bit reply made the driver time out at 3.4s
                    // ("Firmware transaction timeout" cut-here at 2B).
                    // Rates are the Pi 3 nominals the oracle reports.
                    // GET_CLOCK_STATE 0x30001 (req: clock id; resp:
                    // id + on/off): report ON (1), like firmware with
                    // the stock clocks running. Kept ABOVE the clock
                    // arms so the match stays disjoint (Rust rejects
                    // two arms for the same value).
                    0x00030001 => {
                        let mut out = [0u8; 8];
                        out[0..4].copy_from_slice(&self.mem_u32(addr + off as u64 + 12).to_le_bytes());
                        out[4..8].copy_from_slice(&1u32.to_le_bytes());
                        self.mbox_tag_bytes(addr, off, tsize, &out);
                    }
                    // GET_CLOCK_RATE 0x30002 (req: clock id u32; resp:
                    // id + rate Hz). The firmware's V3D quirk reuses the
                    // same tag id as a SET with an empty request
                    // (tsize==0, no id word): echo zeros with success so
                    // the quirk write is absorbed, like real firmware.
                    0x00030002 => {
                        if tsize == 0 {
                            self.mbox_tag_bytes(addr, off, tsize, &[]);
                        } else {
                            let clk = self.mem_u32(addr + off as u64 + 12);
                            let rate = match clk {
                                1 => 700_000_000u32,   // ARM
                                2 => 250_000_000u32,   // CORE
                                4 => 400_000_000u32,   // V3D
                                5 => 250_000_000u32,   // H264
                                6 => 250_000_000u32,   // ISP
                                7 => 250_000_000u32,   // SDRAM
                                8 => 108_000_000u32,   // PIXEL
                                9 => 216_000_000u32,   // PWM
                                _ => 250_000_000u32,
                            };
                            let mut out = [0u8; 8];
                            out[0..4].copy_from_slice(&clk.to_le_bytes());
                            out[4..8].copy_from_slice(&rate.to_le_bytes());
                            self.mbox_tag_bytes(addr, off, tsize, &out);
                        }
                    }
                    // GET_CLOCK_RATE_MEASURED 0x30003 (req: clock id;
                    // resp: id + measured Hz): same nominals, no PLL to
                    // measure against.
                    0x00030003 => {
                        let clk = self.mem_u32(addr + off as u64 + 12);
                        let rate = match clk {
                            1 => 700_000_000u32,
                            2 => 250_000_000u32,
                            4 => 400_000_000u32,
                            _ => 250_000_000u32,
                        };
                        let mut out = [0u8; 8];
                        out[0..4].copy_from_slice(&clk.to_le_bytes());
                        out[4..8].copy_from_slice(&rate.to_le_bytes());
                        self.mbox_tag_bytes(addr, off, tsize, &out);
                    }
                    // GET_MIN_CLOCK_RATE / GET_MAX_CLOCK_RATE 0x30007 /
                    // 0x30004 (req: clock id; resp: id + Hz): floor 0
                    // for min (firmware reports the floor), nominal for
                    // max. The cpufreq driver reads MAX to build its
                    // frequency table; zeros would collapse the table.
                    0x00030004 => {
                        let clk = self.mem_u32(addr + off as u64 + 12);
                        let rate = match clk {
                            1 => 700_000_000u32,
                            2 => 250_000_000u32,
                            4 => 400_000_000u32,
                            _ => 250_000_000u32,
                        };
                        let mut out = [0u8; 8];
                        out[0..4].copy_from_slice(&clk.to_le_bytes());
                        out[4..8].copy_from_slice(&rate.to_le_bytes());
                        self.mbox_tag_bytes(addr, off, tsize, &out);
                    }
                    0x00030007 => {
                        let clk = self.mem_u32(addr + off as u64 + 12);
                        let mut out = [0u8; 8];
                        out[0..4].copy_from_slice(&clk.to_le_bytes());
                        // out[4..8] stays 0 = floor unknown
                        self.mbox_tag_bytes(addr, off, tsize, &out);
                    }
                    // HAS_CLOCK 0x30006 (req: clock id; resp: 1 exists):
                    // the clocks driver skips ids that report absent.
                    0x00030006 => {
                        let clk = self.mem_u32(addr + off as u64 + 12);
                        let mut out = [0u8; 8];
                        out[0..4].copy_from_slice(&clk.to_le_bytes());
                        out[4..8].copy_from_slice(&1u32.to_le_bytes());
                        self.mbox_tag_bytes(addr, off, tsize, &out);
                    }
                    // Notify-firmware-ready 0x30046 (req: 0 words; resp:
                    // none): the firmware-clocks driver sends this after
                    // its clock table is up (proven live: sole tag of the
                    // 2B boot's third request, tsize 4). Absorb with
                    // success (no payload to echo).
                    0x00030046 => {
                        self.mbox_tag_bytes(addr, off, tsize, &[]);
                    }
                    // GET_TURBO 0x30009 (resp: level 0) / SET_TURBO
                    // 0x28001 (req: level; absorbed, level ignored):
                    // stock firmware boots at turbo 0.
                    0x00030009 | 0x00028001 => {
                        self.mbox_tag_bytes(addr, off, tsize, &0u32.to_le_bytes())
                    }
                    // GET_VOLTAGE / GET_MIN/MAX_VOLTAGE 0x30003-class
                    // 0x3000D/0x30010/0x3000E (req: volt id; resp:
                    // id + uV offset): report ~1.2V (1200000+0 offset
                    // encoding the firmware uses: value = uV - 1200000
                    // in 25mV steps is overkill; raw 0 = 1.2V nominal).
                    0x0003000d | 0x0003000e | 0x00030010 => {
                        let idv = self.mem_u32(addr + off as u64 + 12);
                        let mut out = [0u8; 8];
                        out[0..4].copy_from_slice(&idv.to_le_bytes());
                        // out[4..8] stays 0 = 1.2V nominal
                        self.mbox_tag_bytes(addr, off, tsize, &out);
                    }
                    // Unknown tags (M61 triage aid): MBOXTAG env in
                    // mbox_tag_bytes already names every KNOWN tag too,
                    // so this arm stays quiet. Still replies
                    // success+zeros (never the error bit: the clocks
                    // driver treats error as transaction failure and
                    // times out the whole queue).
                    _ => {
                        self.mbox_tag_bytes(addr, off, tsize, &[]);
                    }
                }
            }
            off += 12 + tsize + ((4 - (tsize % 4)) % 4);
        }
        self.mem_write_u32(addr + 4, 0x80000000);
        self.mbx_pending = true;
    }

    /// Framebuffer geometry for the browser canvas blit.
    pub fn fb_geometry(&self) -> (u32, u32, u32, bool) {
        (self.fb_w, self.fb_h, self.fb_pitch, self.fb_ready)
    }

    /// Explicit-done park flag per guest (the browser's runUntil*Done
    /// loops poll this instead of JS model state): 0 = clock/gpio
    /// (TMR+0x20), 1 = mmu (MMU_CTL+0x04), 2 = dma (ENABLE+0x54),
    /// 3 = pwm (+0x54), 4 = i2c (+0x54), 5 = spi (+0x54), 6 = sd (+0x54),
    /// 7 = periphs/debug (USB DONE: +0xFF0 or +0x54).
    /// Window-backed cells read their backing; absorbed writes (spi/sd)
    /// are carried by latches. Anything else reads 0.
    pub fn done_flag(&self, sel: u32) -> u32 {
        let b = match sel {
            0 => self.tmr_done,
            1 => self.mmu_done_cell != 0,
            2 => self.dma_en_back.get(0x54 / 4).copied().unwrap_or(0) != 0,
            3 => self.pwm_back.get(0x54 / 4).copied().unwrap_or(0) != 0,
            4 => self.i2c_back.get(0x54 / 4).copied().unwrap_or(0) != 0,
            5 => self.spi_done,
            6 => self.sd_done,
            7 => self.usb_done,
            _ => false,
        };
        b as u32
    }

    /// One DMA transfer (mirrors dma.js transfer() exactly, including
    /// page-chunking and the IGNORE fills).
    fn dma_transfer(&mut self, ti: u32, src: u64, dst: u64, len: u64) {
        const PAGE: u64 = 4096;
        let src_inc = ti & 1 != 0;
        let dst_inc = ti & 2 != 0;
        let src_ign = ti & (1 << 6) != 0;
        let dst_ign = ti & (1 << 7) != 0;
        let fill = if src_ign && self.in_ram(src, 1) {
            self.mem[src as usize]
        } else {
            0
        };
        let mut s = src;
        let mut d = dst;
        let mut rem = len;
        while rem > 0 {
            let mut chunk = rem;
            let sp = PAGE - (s & (PAGE - 1));
            if sp < chunk {
                chunk = sp;
            }
            let dp = PAGE - (d & (PAGE - 1));
            if dp < chunk {
                chunk = dp;
            }
            let mut buf = vec![0u8; chunk as usize];
            if src_ign {
                for b in buf.iter_mut() {
                    *b = fill;
                }
            } else {
                for (i, b) in buf.iter_mut().enumerate() {
                    let a = s + i as u64;
                    *b = if self.in_ram(a, 1) { self.mem[a as usize] } else { 0 };
                }
                if dst_ign && chunk > 0 {
                    let last = buf[chunk as usize - 1];
                    for b in buf.iter_mut() {
                        *b = last;
                    }
                }
            }
            for (i, b) in buf.iter().enumerate() {
                let a = d + i as u64;
                if self.in_ram(a, 1) {
                    self.mem[a as usize] = *b;
                }
            }
            if src_inc {
                s += chunk;
            }
            if dst_inc {
                d += chunk;
            }
            rem -= chunk;
        }
    }

    /// DMA control-block chain (mirrors dma.js dmaRunChain): walks up
    /// to 64 CBs in guest RAM. Returns true if the last CB had TI.INTEN.
    fn dma_run_chain(&mut self, conblk: u64) -> bool {
        let mut cb = conblk & !0x1f;
        let mut inten = false;
        for _ in 0..64 {
            if cb == 0 || !self.in_ram(cb, 32) {
                break;
            }
            // Snapshot the CB first (the transfer below needs &mut).
            let a = cb as usize;
            let mut raw = [0u8; 32];
            raw.copy_from_slice(&self.mem[a..a + 32]);
            let rd = |o: usize| {
                u32::from_le_bytes([raw[o], raw[o + 1], raw[o + 2], raw[o + 3]])
            };
            let ti = rd(0);
            let src = rd(4) as u64;
            let dst = rd(8) as u64;
            let len = (rd(12) & 0xfffff) as u64;
            if len != 0 {
                self.dma_transfer(ti, src, dst, len);
            }
            if ti & (1 << 31) != 0 {
                inten = true;
            }
            cb = rd(20) as u64 & !0x1f;
        }
        inten
    }

    /// I2C sensor read (mirrors i2c.js slaveRead): WHO_AM_I 0x68,
    /// TEMP 26/0, COUNTER++.
    fn i2c_slave_read(&mut self, reg: u32) -> [u8; 4] {
        let mut r = [0u8; 4];
        match reg {
            0x00 => r[0] = 0x68,
            0x10 => {
                r[0] = 26;
            }
            0x20 => {
                self.i2c_counter = self.i2c_counter.wrapping_add(1);
                r[0] = (self.i2c_counter & 0xff) as u8;
            }
            _ => {}
        }
        r
    }

    /// I2C transfer on C.ST rising edge (mirrors i2c.js syncIn).
    fn i2c_start(&mut self) {
        // DLEN/A latched (masked like the facade; dlen only bounds the
        // write snapshot, the register value selects).
        self.i2c_dlen = self.i2c_back[0x08 / 4] & 0xffff;
        self.i2c_addr = self.i2c_back[0x0c / 4] & 0x7f;
        if self.i2c_c & 1 != 0 {
            // READ transfer: serve the latched register into the FIFO
            // cell (visible once sDone publishes, like the facade's
            // syncOut reload).
            let resp = self.i2c_slave_read(self.i2c_reg);
            self.i2c_back[0x10 / 4] = (resp[0] as u32)
                | ((resp[1] as u32) << 8)
                | ((resp[2] as u32) << 16)
                | ((resp[3] as u32) << 24);
            self.i2c_sdone = true;
        } else {
            // WRITE transfer: first FIFO byte selects the register.
            self.i2c_reg = self.i2c_back[0x10 / 4] & 0xff;
            self.i2c_sdone = true;
        }
    }

    /// SPI flash response per outbound byte index (mirrors spi.js
    /// slaveResponse): byte 0 dummy 0x00, then JEDEC 0xEF/0x40/0x18
    /// for command 0x9F, then 0xFF.
    fn spi_slave_response(cmd: u8, idx: usize) -> u8 {
        if idx == 0 {
            return 0x00;
        }
        if cmd == 0x9f {
            if idx == 1 {
                return 0xef;
            }
            if idx == 2 {
                return 0x40;
            }
            if idx == 3 {
                return 0x18;
            }
        }
        0xff
    }

    /// SPI FIFO push (mirrors spi.js pushTx): append outbound bytes,
    /// extending the response in lockstep.
    fn spi_push_tx(&mut self, size: u64, v: u64) {
        for i in 0..size {
            let b = ((v >> (8 * i)) & 0xff) as u8;
            if self.spi_tx.is_empty() {
                self.spi_cmd = b;
            }
            let idx = self.spi_tx.len();
            self.spi_tx.push(b);
            self.spi_rx.push(Self::spi_slave_response(self.spi_cmd, idx));
        }
    }

    /// GPLEV0 pin levels for the browser LED/button panel (mirrors the
    /// +0x34 read).
    pub fn gpio_lev0(&self) -> u32 {
        (self.gpio_out & !self.gpio_in) | self.gpio_in
    }

    /// Flat SD-card image (sector 0 first) for browser Save/Load.
    pub fn sd_export(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.sd_disk.len() * 512);
        for sec in &self.sd_disk {
            out.extend_from_slice(sec);
        }
        out
    }

    /// Replace the disk image (browser Load). Accepts a whole number of
    /// sectors, capped like the write path.
    pub fn sd_import(&mut self, bytes: &[u8]) -> bool {
        if bytes.is_empty() || bytes.len() % 512 != 0 {
            return false;
        }
        let n = bytes.len() / 512;
        if n > 32 {
            return false;
        }
        self.sd_disk.clear();
        for chunk in bytes.chunks_exact(512) {
            let mut sec = [0u8; 512];
            sec.copy_from_slice(chunk);
            self.sd_disk.push(sec);
        }
        true
    }

    /// UI-safe memory read (RAM only; anything else decodes as zeros,
    /// like the fault panel expects for unmapped windows). Deliberately
    /// avoids Bus::read so peeks never pop FIFOs or advance device state.
    pub fn mem_read_bytes(&self, addr: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        for i in 0..len as u64 {
            let a = addr.wrapping_add(i);
            if self.in_ram(a, 1) {
                out.push(self.mem[a as usize]);
            } else {
                out.push(0);
            }
        }
        out
    }

    /// Side-effect-free device peek for test harnesses (sess READ):
    /// serves the same values as read() for PURE cells (GPIO
    /// FSEL/LEV/EDS/enables); stateful cells (UART DR pop, FIFOs) and
    /// everything else read 0. The wasm mem_read stays RAM-only.
    pub fn peek(&self, addr: u64) -> u32 {
        if addr >= GPIO_BASE && addr + 4 <= GPIO_BASE + 0x1000 {
            return match addr - GPIO_BASE {
                0x00 | 0x04 | 0x08 | 0x0c | 0x10 | 0x14 => {
                    self.gpio_fsel[((addr - GPIO_BASE) / 4) as usize]
                }
                0x34 => (self.gpio_out & !self.gpio_in) | self.gpio_in,
                0x40 => self.gpio_eds,
                _ => 0,
            };
        }
        0
    }

    pub fn read(&mut self, mut addr: u64, size: u64) -> Result<u64, Fault> {
        // M58 hot-path: skip translate() entirely when the MMU is off
        // (all goldens + the kernel's 47k-instruction head.S prologue).
        // With MMU on, the device-window test precedes the walk so MMIO
        // never pays for a page-table walk it bypasses anyway.
        if (self.mmu_sctlr & 1) == 0 {
            if self.in_ram(addr, size) {
                let a = addr as usize;
                let mut v = 0u64;
                for i in 0..size {
                    v |= (self.mem[a + i as usize] as u64) << (8 * i);
                }
                return Ok(v);
            }
        } else {
            addr = self.translate(addr)?;
            if self.in_ram(addr, size) {
                let a = addr as usize;
                let mut v = 0u64;
                for i in 0..size {
                    v |= (self.mem[a + i as usize] as u64) << (8 * i);
                }
                return Ok(v);
            }
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
                0x00 | 0x04 | 0x08 | 0x0c | 0x10 | 0x14 => {
                    self.gpio_fsel[(off / 4) as usize] as u64
                }
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
                _ => 0, // GPPUD/SET/CLR absorb (write-only)
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
        // VideoCore mailbox regs (+0x880 in the MBOX page). MUST precede
        // is_ic: the IC window spans IC_BASE..MBOX_PAGE end, so it would
        // otherwise swallow MBOX reads into its `_ => 0` arm (fb guest
        // then spins past the polls and reports "mailbox failed"). Reads
        // serve the sync_out snapshots. M61 multi-shot: every channel-8
        // MAIL1 write re-processes (no changed-value gate — the kernel
        // reuses one buffer address per probe); pending is the
        // publish flag only (no read-side clear: the fb guest's
        // collect-read at +0x00 must NOT consume state the STATUS poll
        // still needs).
        // Register layout (upstream bcm2835-mailbox.c + BCM2835-ARM-
        // Peripherals 1.3: ARM_0_MAIL0=0x00, ARM_0_MAIL1=0x20):
        // MAIL1_WRT=+0x20, MAIL0_RD=+0x00, MAIL0_STA=+0x18,
        // MAIL1_STA=+0x38. DUAL-DECODE during migration (M61): the
        // in-repo fb/shell guests were written against +0x14
        // (MBOX_MAIL1_WRITE const) + +0x18 MAIL1_STATUS / +0x04 STATUS
        // and their goldens pin that path — the kernel uses the real
        // +0x20/+0x18/+0x38 layout. Serve BOTH: +0x00 READ and +0x18
        // STATUS read the live snapshots; +0x04/+0x14 keep their legacy
        // echoes so the goldens never see a behavior change; +0x20/0x38
        // serve the kernel path. When fb/shell migrate to +0x20, drop
        // the +0x04/+0x14 legacy cells.
        if addr >= MBOX_BASE && addr + size <= MBOX_BASE + 0x40 {
            if std::env::var("MBOXTAG").is_ok() && (addr - MBOX_BASE == 0x00 || addr - MBOX_BASE == 0x18 || addr - MBOX_BASE == 0x38) {
                eprintln!("MBOXRD off=0x{:x} pending={}", addr - MBOX_BASE, self.mbx_pending as u8);
            }
            // M61 STATUS bits (proven by the 2B handshake trace: the
            // kernel driver polls MAIL1_STA (+0x38) 3x and NEVER reads
            // MAIL0 (+0x00) — the old hardwired +0x38=0 ("never full")
            // meant EMPTY-always and the reply was never collected, so
            // every transaction timed out at 3.4s. Real STA layout:
            // bit31 FULL + bit30 EMPTY. MAIL1 (ARM->VC) is EMPTY when
            // idle (nothing queued) and NOT EMPTY (FULL=1 if the FIFO
            // depth is 1) while a request is in flight; MAIL0 (VC->ARM)
            // is NOT EMPTY while the reply waits. The fb/shell/debug
            // guests use the LEGACY cells (+0x04 STATUS echo +
            // always-clear +0x18-legacy), which keep their exact old
            // values — only the kernel-path +0x38/+0x18 cells change.
            // +0x18 MAIL0_STA keeps the legacy always-clear read (the
            // debug golden pins it at 0 and the kernel never reads it —
            // proven: 0 MBOXRD +0x18 lines at 2B).
            // M61b (proven: with FULL=1-while-pending the 2B run did 1
            // txn then 5458 EMPTY=0 polls = driver waiting for its OWN
            // queue slot back, never collecting): the driver treats the
            // queue as depth-1 — it needs EMPTY=1 (slot back) once the
            // reply is readable. So while pending: FULL=0, EMPTY=1
            // (request absorbed, reply ready at MAIL0). Idle: EMPTY=1.
            // M61c IRQ path (upstream bcm2835-mailbox.c, replaces the
            // STATUS-poll theory above): the driver NEVER polls STA —
            // txdone polls MAIL1 FULL (last_tx_done) and completion
            // arrives via the MAIL0 IRQ. M61d correction (execution:
            // EMPTY=1-always polled 3x then timed out — the driver was
            // never going to collect via STATUS): while pending, MAIL1
            // reads NOT-EMPTY (EMPTY bit SET would claim "slot back"
            // while the reply is still unread — the txdone poll must see
            // FULL until the IRQ handler drains MAIL0_RD). So pending:
            // FULL=0, EMPTY=0 (busy, reply waits); idle: EMPTY=1. The
            // +0x00 READ drains (clears pending) regardless of STA.
            // M64 QUEUE-DEPTH-1 (execution-proven 2026-09-19): real HW
            // has a 1-deep MAIL1 FIFO — while a request is in flight
            // the FIFO is FULL (bit31=1), and EMPTY (bit30) reads 0.
            // (First cut set FULL=1 here and the driver went single-
            // flight: 1 MBOXWR then 5458 EMPTY=0 polls awaiting its own
            // queue slot back, never collecting — the txdone poll is a
            // spin, not an IRQ kick. So FULL=1 alone is NOT the answer;
            // the completion must ALSO be collectible. Kept FULL=0/
            // EMPTY=0 busy-while-pending; the real fix is the drain
            // word + line drop below.)
            // M66 TX-DONE SPIN (execution-proven 2026-09-19, current-2B
            // trace): the firmware xact waiter does NOT sleep on an IRQ
            // at all -- it spins on the tx-done poll
            // (`...0207b0/020704` MBOXSEND sites are INSIDE the
            // `...0206c4` loop whose only exits are the `...0207dc`
            // drain-collect path and the `...0207f4` idc-match path).
            // HYPOTHESIS (txdone reads MAIL1_STA, needs EMPTY=1 to stop
            // spinning): pending should read EMPTY=1 (FULL=0). TRIED
            // (`mail1_sta = 1<<30` unconditionally) → stall IDENTICAL
            // (same pc/tail/3 polls, /tmp/opencode/m66spin.*) — so the
            // poll is NOT gated on our STA bits, or the loop never
            // reaches the poll (pre-send idc-miss spins first — see
            // M66 idc verdict below). Kept as unconditional EMPTY=1:
            // matches the "slot back once reply readable" model and is
            // battery-green; revisit only with a trace showing the poll
            // consuming it.
            let mail1_sta: u64 = 1 << 30;
            // MAIL0_STA while pending must read NON-EMPTY (EMPTY bit
            // clear) so the IRQ handler's while-loop enters and drains
            // the reply (see the +0x00 arm, which clears pending).
            // M61d: same busy-while-pending as MAIL1 (see above).
            // NOTE: this changes the legacy always-clear +0x18 read —
            // the debug golden pins +0x18 idle 0, which still holds
            // (idle EMPTY=1<<30 has bit31 clear; the golden masks
            // 0x80000000). fb/shell never touch +0x18.
            let mail0_sta: u64 = if self.mbx_pending { 0 } else { 1 << 30 };
            let v: u64 = match addr - MBOX_BASE {
                // MAIL0_RD (+0x00): the IRQ handler's drain read (see
                // bcm2835_mbox_irq: while MAIL0_STA !EMPTY, read MAIL0_RD
                // + mbox_chan_received_data = complete()). Consumption is
                // synchronous here (single-word reply): the read returns
                // the reply word and clears pending, which drops the
                // bank-0 line AND raises EMPTY on both STA cells. The
                // fb/shell collect-reads hit the same arm (their reply
                // word is the buffer PA | channel) — unchanged behavior.
                // M61 drain trace (MBOXTAG=1): every drain names its
                // source register read so the chained-handler walk is
                // proven by execution (MAIL0_RD must follow an IC
                // BASIC/PENDING read in the same unmasked window).
                0x00 => {
                    // M64 drain fix (execution-proven 2026-09-19): the old
                    // arm returned the STALE `mbx_pub_read` snapshot (last
                    // published word) instead of the CURRENT request's
                    // word (`mbx_last_write`). Multi-shot reuses one
                    // buffer per probe, so after request #1 drained,
                    // requests #2/#3 drained the SAME stale word: the tx
                    // poll saw its slot back but the reply never matched,
                    // and every firmware transaction timed out at 3.4s.
                    // Serve the live word; keep the pending-clear + line
                    // drop + STA-raise behavior unchanged.
                    let w = self.mbx_last_write as u64;
                    if std::env::var("MBOXTAG").is_ok() && self.mbx_pending {
                        eprintln!("MBOXDRAIN rd=0x{:08x}", self.mbx_last_write);
                    }
                    self.mbx_pending = false;
                    w
                }
                0x04 => self.mbx_pub_status as u64, // legacy STATUS echo (fb/shell poll FULL: never full here)
                0x14 => self.mbx_last_write as u64, // legacy WRITE echo (fb/shell)
                0x18 => mail0_sta, // MAIL0_STA: NON-EMPTY while a reply waits (IRQ drain path); EMPTY when idle
                0x20 => self.mbx_last_write as u64, // MAIL1_WRT echoes (write-only on HW)
                0x38 => mail1_sta, // MAIL1_STA: EMPTY when idle (kernel poll path)
                _ => 0,
            };
            return Ok(v & mask(size));
        }
        if Self::is_ic(addr, size) {
            let off = addr - IC_BASE;
            // M61 IRQ-chain trace (MBOXTAG=1): counts BASIC/PENDING reads
            // so the chained-handler walk is proven by execution (LOCAL
            // +0x60 -> IC BASIC -> PENDING1 -> MAIL0_RD).
            // M62h step-(b) WIDE LOG (proven: the ONLY IC reads in 2B are
            // 4 boot-time ENABLE-mirror reads at +0x18/+0x10/+0x14/+0x0c
            // with mbox0=0 — the handler issues ZERO IC reads of any
            // offset/size in all 11358 mailbox-visible windows, so the
            // GPU half genuinely never walks while the timer half is
            // pending; serve-or-complete decision goes to (a)).
            if std::env::var("MBOXTAG").is_ok() {
                // value computed below; log after (see ICVALW line).
            }
            // M61 BASIC snapshot trace (MBOXTAG=1): the VALUE the guest
            // saw, so a zero-BASIC read with a live line is proven (not
            // inferred) and vice versa.
            let v: u64 = match off {
                // PENDING2 (GPIO bit 17 + UART bit 25) + BASIC mirrors
                // (bit 9 for GPIO, bit 19 shortcut for UART) + bank-0
                // bit 1 for MAILBOX (M61 IRQ path, DTB-proven) + bank-1
                // bit 9 for USB (M67: INTERRUPT_USB = GPU IRQ 9, served
                // in PENDING1; BASIC bit 8 mirrors any non-shortcut
                // bank-1 line incl. USB, like timer/DMA).
                0x08 => (self.gpio_pending2() | self.uart_pending2()) as u64,
                0x04 => (self.timer_pending1() | self.dma_pending1() | self.usb_pending1()) as u64,
                0x00 => {
                    let mut b = 0u64;
                    if self.timer_pending1() | self.dma_pending1() | self.usb_pending1() != 0 {
                        b |= 1 << 8; // any non-shortcut bank-1 line
                    }
                    if self.gpio_pending2() != 0 {
                        b |= 1 << 9;
                    }
                    if self.uart_pending2() != 0 {
                        b |= 1 << 19;
                    }
                    if self.mbox_pending0() != 0 {
                        b |= 1 << 1; // ARM_MAILBOX (bank-0 bit 1, DTB-proven)
                    }
                    b
                }
                _ => 0, // ENABLE/DISABLE/RET read back 0
            };
            if std::env::var("MBOXTAG").is_ok() {
                eprintln!("ICRDW off=0x{:x} size={} val=0x{:x} mbox0={}", off, size, v & mask(size), self.mbox_pending0());
            }
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
        if Self::is_i2c(addr, size) {
            // BSC registers (mirrors i2c.js syncOut publishes): C shows
            // latched I2CEN|READ plus ST while a transfer is done; S shows
            // DONE; DLEN/A/FIFO/DONE read the window backing.
            let off = addr - I2C_BASE;
            let v: u64 = match off {
                // Published snapshots (see sync_out): the guest only ever
                // observes slice-boundary state, exactly like the facade.
                0x00 => self.i2c_pub_c as u64,
                0x04 => self.i2c_pub_s as u64,
                _ => {
                    let idx = (off / 4) as usize;
                    if idx < self.i2c_back.len() {
                        self.i2c_back[idx] as u64
                    } else {
                        0
                    }
                }
            };
            return Ok(v & mask(size));
        }
        if Self::is_spi(addr, size) {
            // SPI0 CS (mirrors spi.js syncOut): TA latch, TXD (always
            // drained), RXD while response bytes remain, DONE when ready.
            // FIFO reads the staged response backing.
            let off = addr - SPI_BASE;
            let v: u64 = match off {
                // Published snapshot (see sync_out), unless the guest
                // wrote CS since (window shows the write until publish).
                0x00 => self.spi_cs_dirty.unwrap_or(self.spi_pub_cs) as u64,
                0x04 => self.spi_fifo_le(),
                _ => 0,
            };
            return Ok(v & mask(size));
        }
        // PWM window backing (CTL/STA published at sync_out).
        if Self::is_pwm(addr, size) {
            let mut w = 0u64;
            for i in 0..size {
                let o = (addr - PWM_BASE) + i;
                let cell = self.pwm_back.get((o / 4) as usize).copied().unwrap_or(0);
                w |= (((cell >> (8 * (o % 4))) & 0xff) as u64) << (8 * i);
            }
            return Ok(w & mask(size));
        }
        if Self::is_miniuart(addr, size) {
            // Window backing (guest writes persist; ENABLES/LSR/IO are
            // overwritten by sync_out, exactly like the facade window).
            let idx = ((addr - UART1_BASE) / 4) as usize;
            let v = if idx < self.uart1_back.len() {
                self.uart1_back[idx] as u64
            } else {
                0
            };
            return Ok(v & mask(size));
        }
        // M30 windows (periphs/debug guests): RNG CTRL latch + fixed
        // 45.0 C temp DATA (mirrors rng.js/temp.js), zero CLK/I2S/I2C0
        // windows, AUX UART2-5 (ENABLES latch + live LSR, mirrors
        // uart25.js), USB GSNPSID + DONE park (mirrors usb.js).
        if Self::is_rng(addr, size) {
            let v: u64 = match addr - RNG_BASE {
                0x00 => self.rng_ctrl as u64,
                0x04 => 45000,
                _ => 0,
            };
            return Ok(v & mask(size));
        }
        if Self::is_clk(addr, size) || Self::is_i2s(addr, size) || Self::is_i2c0(addr, size) {
            return Ok(0); // untouched windows read zero, like the facade
        }
        if Self::is_uart25(addr, size) {
            let bases = [UART2_BASE, UART3_BASE, UART4_BASE, UART5_BASE];
            let mut k = 0usize;
            for (i, b) in bases.iter().enumerate() {
                if addr >= *b {
                    k = i;
                }
            }
            let off = addr - bases[k];
            let v: u64 = match off {
                // LSR served live (mirrors uart25.js syncOut): TX_EMPTY
                // + TX_IDLE once enabled, else 0.
                0x54 => {
                    if self.uart25_enabled[k] {
                        (1 << 5) | (1 << 6)
                    } else {
                        0
                    }
                }
                _ => {
                    let idx = (off / 4) as usize;
                    self.uart25_back[k].get(idx).copied().unwrap_or(0) as u64
                }
            };
            return Ok(v & mask(size));
        }
        if Self::is_usb(addr, size) {
            // M67 DWC2 OTG core (QEMU hcd-dwc2.c register semantics):
            // glb 0x000-0x06C shadow cells, HPTXFSIZ at 0x100, host
            // 0x400-0x440 cells, channels 0x500+ (HCCHAR/HCSPLT/HCINT/
            // HCINTMSK/HCTSIZ/HCDMA/HCDMAB per 0x20 stride, 8 channels).
            // GRSTCTL reads mask the self-clearing bits (QEMU glbreg
            // read arm); HFNUM serves frame+FRREM live; HAINT/HPRT0
            // serve shadow cells; GSNPSID reads 0x4f54294a (QEMU reset
            // value — periphs/debug accept it as a real DWC2 rev).
            let off = addr - USB_BASE;
            if std::env::var("USBTRACE").is_ok() && (off < 0x70 || off == 0x100 || (0x400..0x444).contains(&off) || (0x500..0x600).contains(&off)) {
                eprintln!("USBRD off=0x{:x} size={}", off, size);
            }
            let v: u64 = if off < 0x70 && off % 4 == 0 {
                let mut w = self.usb_glb[(off / 4) as usize] as u64;
                if off == 0x10 {
                    // GRSTCTL: self-clearing bits never read back
                    // (QEMU dwc2_glbreg_read GRSTCTL arm verbatim).
                    w &= !((1 << 5) | (1 << 4) | (1 << 3) | (1 << 2) | (1 << 1) | 1);
                }
                w
            } else if off == 0x100 {
                500 << 16 // HPTXFSIZ (QEMU fszreg reset value)
            } else if (0x400..0x444).contains(&off) && off % 4 == 0 {
                if off == 0x408 {
                    // HFNUM: live frame + FRREM (QEMU hreg0 arm:
                    // FRREM = remaining clocks in frame; pi-cpu has no
                    // SOF clock, so report FRREM = full window).
                    ((0x2edcu64) << 16) | ((self.usb_frame & 0x3fff) as u64)
                } else {
                    self.usb_hreg0[((off - 0x400) / 4) as usize] as u64
                }
            } else if (0x500..0x600).contains(&off) && off % 4 == 0 {
                let ch = ((off - 0x500) / 0x20) as usize;
                let reg = ((off - 0x500) % 0x20) / 4;
                if ch < 8 && reg < 8 {
                    self.usb_hch[ch][reg as usize] as u64
                } else {
                    0
                }
            } else {
                0
            };
            return Ok(v & mask(size));
        }
        // DMA ch0 + ENABLE page: full window backing (the facade
        // windows are RAM — guest writes read back until sync_out
        // overwrites CS; sync_in pulls CS/CONBLK/ENABLE from here).
        // Byte-assembled so unaligned/partial accesses match RAM.
        if Self::is_dma(addr, size) {
            let base = if addr >= DMA_ENABLE_PAGE {
                DMA_ENABLE_PAGE
            } else {
                DMA_BASE
            };
            let mut w = 0u64;
            for i in 0..size {
                let o = addr - base + i;
                let cell = if addr >= DMA_ENABLE_PAGE {
                    self.dma_en_back.get((o / 4) as usize).copied().unwrap_or(0)
                } else {
                    self.dma_back.get((o / 4) as usize).copied().unwrap_or(0)
                };
                w |= (((cell >> (8 * (o % 4))) & 0xff) as u64) << (8 * i);
            }
            return Ok(w & mask(size));
        }
        // MMU_CTL compat (mirrors the host-assisted model): MMU_CTL
        // echoes (the guest polls bit0 from its own write), DONE parks.
        if Self::is_mmuctl(addr, size) {
            let off = addr - MMU_CTL;
            let v: u64 = match off {
                0x00 => self.mmu_ctl_cell as u64,
                0x04 => self.mmu_done_cell as u64,
                _ => 0,
            };
            return Ok(v & mask(size));
        }
        // SMP spin-table window: plain byte backing (the SmpShared
        // arbiter in runner.rs mirrors it per core per chunk).
        if Self::is_smp(addr, size) {
            let mut w = 0u64;
            for i in 0..size {
                let o = addr - SMP_BASE + i;
                w |= ((self.smp_mem.get(o as usize).copied().unwrap_or(0) as u64) << (8 * i));
            }
            return Ok(w & mask(size));
        }
        if Self::is_page(addr, size, MBOX_PAGE) || Self::is_page(addr, size, LOCAL_BASE) {
            // Local block CORE_IRQ_SRC (core 0, +0x60): bit 1 = CNTPNSIRQ
            // (gated by the LOCAL_TIMER_INT_CONTROL0 bit1 enable — the
            // kernel's clocksource tick unmasks ONLY its own timer; a raw
            // counter-compare with the enable clear must NOT report the
            // bit, or the chained handler dispatches a dead timer IRQ),
            // bit 8 = GPU (legacy line, gated by GPU_ROUTING==0 — the
            // kernel writes 0x0 at boot, proven by LOCALWR trace; nonzero
            // would steer GPU IRQs to FIQ/local, unmodeled). GPU_ROUTING
            // itself (+0x0C) reads back the latch. Timer/mailbox control
            // cells (+0x40/+0x50) read back their latches. Everything else
            // zero.
            // M61 IRQ-chain trace (MBOXTAG=1): counts CORE_IRQ_SRC reads
            // so the handler walk is proven by execution (the chained
            // handler must read +0x60 to find the GPU bit, then the IC
            // BASIC/PENDING1 to find bank-0 bit 1, then MAIL0_RD).
            if Self::is_page(addr, size, LOCAL_BASE) && addr - LOCAL_BASE == 0x60 {
                let mut v = 0u64;
                // M61 BARE-METAL COMPAT (lirq/rpi-kernel green): the
                // bare-metal guests never program LOCAL_TIMER_INT_CONTROL0
                // (no local-block driver — they use the raw timer), so a
                // hard gate on the enable bit would starve them (proven:
                // lirq Phase A + rpi-kernel timer ticks died with the
                // gate). Gate by the LINUX condition only in linux_mode;
                // bare-metal keeps the legacy raw-compare behavior.
                // Upstream truth stands: Linux's tick unmasks bit 1 at
                // +0x40 and the handler dispatches on the reported bit.
                // M62 TIMER REPORT (DTB-oracle + code-read): the DTB's
                // /timer node is `arm,armv7-timer` with interrupt-parent
                // = local_intc and PPI interrupts — the arch timer is a
                // LOCAL line (irq-bcm2836.c LOCAL_IRQ_CNT*), never a
                // legacy armctrl bank-1 line. The +0x60 READ arm and the
                // runner DELIVERY gate use the same `cntp_ok` condition
                // (local-enable && cntp_line incl. IMASK), so a reported
                // bit1 always means a deliverable timer IRQ and vice
                // versa — no report/deliver skew by construction.
                // (An earlier comment revision claimed reporting bit1
                // for a quiet line caused the 11358-no-ICRD stall; the
                // timeronly run disproved it — identical LOCALRD
                // val=0x102 ×11358 with the comment-only change and
                // still ICRD=0. The stall is elsewhere: see M62h.)
                let cntp_ok = if self.linux_mode {
                    (self.local_timer_ctl0 & (1 << 1)) != 0 && self.cntp_line()
                } else {
                    self.cntp_line()
                };
                if cntp_ok {
                    v |= 1 << 1;
                }
                if self.local_gpu_routing == 0 && self.legacy_line() {
                    v |= 1 << 8;
                }
                if std::env::var("MBOXTAG").is_ok() {
                    eprintln!("LOCALRD off=0x60 val=0x{:x} legacy={} cntp={}", v & mask(size), self.legacy_line() as u8, self.cntp_line() as u8);
                }
                return Ok(v & mask(size));
            }
            if Self::is_page(addr, size, LOCAL_BASE) && addr - LOCAL_BASE == 0x0c {
                return Ok(self.local_gpu_routing as u64 & mask(size));
            }
            if Self::is_page(addr, size, LOCAL_BASE) && addr - LOCAL_BASE == 0x40 {
                return Ok(self.local_timer_ctl0 as u64 & mask(size));
            }
            if Self::is_page(addr, size, LOCAL_BASE) && addr - LOCAL_BASE == 0x50 {
                return Ok(self.local_mbox_ctl0 as u64 & mask(size));
            }
            return Ok(0); // unmodeled cells read zero, like the facade
        }
        // Outside the default-mapped set: fault, like the unicorn core.
        // M57 Linux-track note: in linux_mode the translated kernel PA
        // below is ALWAYS < 512M (translate() output), so an
        // UnmappedData here with a 0xffffffxx VA means the WALK produced
        // a bad PA (stale tables/IDC), not a missing window — check the
        // walk_dump before adding windows.
        Err(Fault::UnmappedData(addr))
    }

    pub fn write(&mut self, mut addr: u64, size: u64, val: u64) -> Result<(), Fault> {
        // M58 hot-path: same MMU-off fast path as read().
        if (self.mmu_sctlr & 1) == 0 {
            if self.in_ram(addr, size) {
                // M63 write-watch (zero-cost unless armed): log any RAM
                // store overlapping the watched PA range BEFORE applying
                // it (old bytes still in place). Caller appends the pc.
                if self.wwatch_on
                    && addr < self.wwatch_pa + self.wwatch_len
                    && addr + size > self.wwatch_pa
                {
                    let mut old = 0u64;
                    for i in 0..self.wwatch_len.min(8) {
                        let a = self.wwatch_pa + i;
                        if self.in_ram(a, 1) {
                            old |= (self.mem[a as usize] as u64) << (8 * i);
                        }
                    }
                    if std::env::var("WWATCH").is_ok() {
                        eprintln!(
                            "WWATCH pa=0x{:x} size={} val=0x{:x} old=0x{:x}",
                            addr, size, val & mask(size), old
                        );
                    }
                    self.wwatch_hits += 1;
                }
                let a = addr as usize;
                for i in 0..size {
                    self.mem[a + i as usize] = ((val >> (8 * i)) & 0xff) as u8;
                }
                return Ok(());
            }
        } else {
            addr = self.translate(addr)?;
            if self.in_ram(addr, size) {
                // M63 write-watch (MMU-on path): same as above, on the
                // translated PA.
                if self.wwatch_on
                    && addr < self.wwatch_pa + self.wwatch_len
                    && addr + size > self.wwatch_pa
                {
                    let mut old = 0u64;
                    for i in 0..self.wwatch_len.min(8) {
                        let a = self.wwatch_pa + i;
                        if self.in_ram(a, 1) {
                            old |= (self.mem[a as usize] as u64) << (8 * i);
                        }
                    }
                    if std::env::var("WWATCH").is_ok() {
                        eprintln!(
                            "WWATCH pa=0x{:x} size={} val=0x{:x} old=0x{:x}",
                            addr, size, val & mask(size), old
                        );
                    }
                    self.wwatch_hits += 1;
                }
                let a = addr as usize;
                for i in 0..size {
                    self.mem[a + i as usize] = ((val >> (8 * i)) & 0xff) as u8;
                }
                return Ok(());
            }
        }
        // M60 MMIO-write census: count AFTER translation (translated PA),
        // before dispatch — every guest MMIO write lands in exactly one
        // bucket. Zero-cost when mmio_census is off (triage sets it).
        if self.mmio_census {
            // NOTE: mailbox must bucket BEFORE is_ic (same overlap rule
            // as dispatch: MBOX 0x3F00B880 sits inside IC range; the
            // mailbox window is +0x40 wide: +0x00..+0x3C regs).
            if addr >= MBOX_BASE && addr + size <= MBOX_BASE + 0x40 {
                self.census_mbox += 1;
            } else if Self::is_uart(addr, size) {
                self.census_uart += 1;
            } else if Self::is_timer(addr, size) {
                self.census_tmr += 1;
            } else if Self::is_gpio(addr, size) {
                self.census_gpio += 1;
            } else if Self::is_ic(addr, size) {
                self.census_ic += 1;
            } else if Self::is_sd(addr, size) {
                self.census_sd += 1;
            } else if Self::is_page(addr, size, LOCAL_BASE) {
                self.census_local += 1;
                if std::env::var("MBOXTAG").is_ok() {
                    eprintln!("LOCALWR off=0x{:x} val=0x{:x}", addr - LOCAL_BASE, val & mask(size));
                }
            } else if Self::is_miniuart(addr, size) {
                self.census_miniuart += 1;
            } else if Self::is_i2c(addr, size) {
                self.census_i2c += 1;
            } else if Self::is_spi(addr, size) {
                self.census_spi += 1;
            } else if Self::is_pwm(addr, size) {
                self.census_pwm += 1;
            } else if Self::is_smp(addr, size) {
                self.census_smp += 1;
            } else if Self::is_mmuctl(addr, size) {
                self.census_mmu += 1;
            } else if Self::is_rng(addr, size)
                || Self::is_clk(addr, size)
                || Self::is_i2s(addr, size)
                || Self::is_i2c0(addr, size)
                || Self::is_uart25(addr, size)
                || Self::is_usb(addr, size)
                || Self::is_page(addr, size, MBOX_PAGE)
            {
                self.census_misc += 1;
            }
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
                0x00 | 0x04 | 0x08 | 0x0c | 0x10 | 0x14 => {
                    self.gpio_fsel[(off / 4) as usize] = v;
                }
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
        // VideoCore mailbox: MAIL1_WRT (+0x20, kernel path) latches +
        // processes channel-8 requests (mirrors syncMailboxIn). M61
        // multi-shot: NO changed-value gate (the old gate dropped the
        // kernel's repeated same-buffer writes — every firmware/clock
        // probe reuses one buffer address) and mbox_process itself
        // re-arms per write (pending is the READ-collection flag, not a
        // process latch). DUAL-DECODE during migration: +0x14 (the
        // in-repo fb/shell MBOX_MAIL1_WRITE const) processes identically
        // so their goldens pin the migration — when they move to +0x20,
        // drop the +0x14 arm. Layout per upstream bcm2835-mailbox.c (see
        // read path). Other cells absorb. MUST precede is_ic (same
        // overlap as the read path — the mailbox words would be absorbed
        // as IC enable).
        if addr >= MBOX_BASE && addr + size <= MBOX_BASE + 0x40 {
            if addr - MBOX_BASE == 0x20 || addr - MBOX_BASE == 0x14 {
                let v = (val & mask(size)) as u32;
                self.mbx_last_write = v;
                self.mbx_addr = v;
                if std::env::var("MBOXTAG").is_ok() {
                    eprintln!("MBOXWR off=0x{:x} val=0x{:08x} ch={}", addr - MBOX_BASE, v, v & 0xf);
                }
                if (v & 0xf) == 8 {
                    self.mbox_process(v);
                }
            } else if addr - MBOX_BASE == 0x1c {
                // MAIL0_CNF (+0x1C): interrupt-enable latch (upstream
                // bcm2835_startup writes IHAVEDATAIRQEN=1 at probe,
                // shutdown writes 0). While set, a pending reply raises
                // bank-0 bit 1 (see mbox_pending0) — the driver's real
                // completion path. Other bits absorbed.
                self.mbx_cnf_irqen = (val & 1) != 0;
                if std::env::var("MBOXTAG").is_ok() {
                    eprintln!("MBOXCNF val=0x{:x} irqen={}", val & mask(size), self.mbx_cnf_irqen as u8);
                }
            }
            return Ok(());
        }
        if Self::is_ic(addr, size) {
            let off = addr - IC_BASE;
            let v = (val & mask(size)) as u32;
            if std::env::var("MBOXTAG").is_ok() && (off == 0x18 || off == 0x10 || off == 0x14 || off == 0x24 || off == 0x1c || off == 0x20 || off == 0x00) {
                eprintln!("ICWR off=0x{:x} val=0x{:08x}", off, v);
            }
            match off {
                // Upstream layout (reg_enable[] = {0x18, 0x10, 0x14},
                // reg_disable[] = {0x24, 0x1C, 0x20}): bank-0 enable at
                // +0x18 / disable at +0x24 (the old code mapped +0x10 to
                // bank 1 and never served bank 0 — the mailbox enable
                // vanished into the void).
                0x18 => self.ic_en0 |= v,
                0x10 => self.ic_en1 |= v,
                0x14 => self.ic_en2 |= v,
                0x24 => self.ic_en0 &= !v,
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
                0x54 => self.sd_done = v != 0, // DONE host extension park flag
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
        if Self::is_i2c(addr, size) {
            // C writes: CLEAR edge resets, ST rising edge runs the
            // transfer (mirrors i2c.js syncIn; synchronous here is
            // equivalent — the guest only observes published S/CS).
            // Other cells merge into the window backing.
            let off = addr - I2C_BASE;
            if off == 0x00 {
                let v = (val & mask(size)) as u32;
                if v & (1 << 4) != 0 {
                    self.i2c_sdone = false;
                    self.i2c_resp = [0; 4];
                }
                let start = (v & (1 << 7)) != 0 && (self.i2c_c & (1 << 7)) == 0;
                self.i2c_c = v & ((1 << 15) | (1 << 0) | (1 << 7));
                if start {
                    self.i2c_start();
                }
            } else {
                let idx = (off / 4) as usize;
                if idx < self.i2c_back.len() {
                    let base = ((off) % 4) as u32;
                    let mut cell = self.i2c_back[idx];
                    for i in 0..size {
                        let sh = 8 * (base + i as u32);
                        if sh < 32 {
                            cell &= !(0xff << sh);
                            cell |= (((val >> (8 * i)) & 0xff) as u32) << sh;
                        }
                    }
                    self.i2c_back[idx] = cell;
                }
            }
            return Ok(());
        }
        if Self::is_spi(addr, size) {
            // CS writes: CLEAR edge resets the session, TA rising edge
            // runs the transfer (mirrors spi.js hooks, synchronous here).
            // FIFO writes push TX bytes (response extends in lockstep).
            let off = addr - SPI_BASE;
            if off == 0x00 {
                let v = (val & mask(size)) as u32;
                self.spi_cs_dirty = Some(v);
                if v & (0b11 << 4) != 0 {
                    self.spi_tx.clear();
                    self.spi_rx.clear();
                    self.spi_cmd = 0;
                    self.spi_sdone = false;
                }
                let ta = (v & (1 << 7)) != 0;
                if ta && !self.spi_ta {
                    self.spi_ta = true;
                    if !self.spi_tx.is_empty() {
                        self.spi_sdone = true;
                        // Stage the response into the FIFO backing NOW
                        // (synchronously with sDone — the guest observes
                        // DONE live mid-chunk, so deferred sync_out staging
                        // would serve a stale window; cf. the I2C model
                        // which stages at transfer time).
                        for i in 0..4 {
                            self.spi_fifo[i] = *self.spi_rx.get(i).unwrap_or(&0);
                        }
                    }
                }
                if !ta {
                    self.spi_ta = false;
                }
            } else if off == 0x04 {
                self.spi_push_tx(size, val);
            } else if off == 0x54 {
                self.spi_done = val != 0; // DONE host extension park flag
            }
            return Ok(());
        }
        if Self::is_miniuart(addr, size) {
            // Window backing with RMW (the facade window is RAM). MU_IO
            // (+0x40) TX bytes tap the console with the "[u1] " line tag
            // (uart1Emit rule); the pulse-clear happens in sync_out.
            let idx = ((addr - UART1_BASE) / 4) as usize;
            if idx < self.uart1_back.len() {
                let mut cell = self.uart1_back[idx];
                // Merge the written bytes (little-endian partial writes).
                let base = ((addr - UART1_BASE) % 4) as u32;
                for i in 0..size {
                    let sh = 8 * (base + i as u32);
                    if sh < 32 {
                        cell &= !(0xff << sh);
                        cell |= (((val >> (8 * i)) & 0xff) as u32) << sh;
                    }
                }
                self.uart1_back[idx] = cell;
                if addr - UART1_BASE == 0x40 {
                    let b = (cell & 0xff) as u8;
                    if b != 0 {
                        self.uart1_tx(b);
                    }
                }
            }
            return Ok(());
        }
        // M30 windows: RNG CTRL latch (DATA is the fixed temp read);
        // CLK/I2S/I2C0 absorb; AUX UART2-5 window backing + ENABLES
        // latch (mirrors uart25.js syncIn: sticky on any nonzero write);
        // USB park (periphs writes +0xFF0, debug writes +0x54 — either
        // parks the guest, like the facade's USB_DONE).
        if Self::is_rng(addr, size) {
            if addr - RNG_BASE == 0x00 {
                self.rng_ctrl = (val & mask(size)) as u32;
            }
            return Ok(());
        }
        if Self::is_clk(addr, size) || Self::is_i2s(addr, size) || Self::is_i2c0(addr, size) {
            return Ok(());
        }
        if Self::is_uart25(addr, size) {
            let bases = [UART2_BASE, UART3_BASE, UART4_BASE, UART5_BASE];
            let mut k = 0usize;
            for (i, b) in bases.iter().enumerate() {
                if addr >= *b {
                    k = i;
                }
            }
            let off = addr - bases[k];
            let idx = (off / 4) as usize;
            if let Some(cell) = self.uart25_back[k].get_mut(idx) {
                let base = (off % 4) as u32;
                for i in 0..size {
                    let sh = 8 * (base + i as u32);
                    if sh < 32 {
                        *cell &= !(0xff << sh);
                        *cell |= (((val >> (8 * i)) & 0xff) as u32) << sh;
                    }
                }
                if off == 0x04 && *cell != 0 {
                    self.uart25_enabled[k] = true;
                }
            }
            return Ok(());
        }
        if Self::is_usb(addr, size) {
            // M67 DWC2 write path (QEMU hcd-dwc2.c glbreg/hreg0/hreg1
            // write arms + facade usb.js syncIn edges):
            // - USB_DONE park compat: periphs writes +0xFF0, debug
            //   writes +0x54 (either parks those guests — KEPT so their
            //   goldens never move; the real DWC2 regs at those offsets
            //   are GLPMCFG/HCCHAR0 and neither guest enables the core).
            // - GOTGCTL: read-only bits (BSESVLD|ASESVLD|CONID_B|
            //   HSTNEGSCS|SESREQSCS) preserved; SESREQ rising ->
            //   SESREQSCS + GOTGINT SES_REQ_SUC + OTGINT; HNPREQ rising
            //   -> HSTNEGSCS + GOTGINT HST_NEG_DET + OTGINT.
            // - GINTSTS: W1C (QEMU arm verbatim: val|=~old, val=~val,
            //   then re-set read-only bits) + irq recompute.
            // - GINTMSK/GAHBCFG: latch (+ GBL_INTR_EN edge wakes sync).
            // - GRSTCTL: AHBIDLE forced; CSFTRST/HSFTRST self-clear +
            //   restore reset GINTSTS/GOTGCTL; RXFFLSH/TXFFLSH clear
            //   their status bits.
            // - HPRT0: read-only bits preserved (SPD/LNSTS/OVRCURRACT/
            //   CONNSTS), SUSP/RES preserved, ENA never set directly;
            //   PRTRST falling with device attached -> ENA|ENACHG +
            //   PRTINT; W1C bits (OVRCURRCHG|ENACHG|ENA|CONNDET);
            //   PPWR set -> CONNSTS|CONNDET + PRTINT (LAN7800 attach).
            // - HCCHAR: CHDIS rising -> clear CHENA + HCINT.CHHLTD;
            //   CHENA rising -> usb_xfer (sync completion) + irq walk.
            // - HCINT: W1C; HCINTMSK: mask latch (RESERVED14_31 kept 0).
            // - HCTSIZ/HCDMA/HCSPLT/HCDMAB: latch.
            // - GOTGINT/GUID/GHWCFG*/GRXFSIZ/GNPTXFSIZ*/FIFO regs:
            //   plain latches (ID/config are guest-programmable sizes).
            let off = addr - USB_BASE;
            if (off == 0xff0 || off == 0x54) && val != 0 {
                self.usb_done = true;
            }
            if std::env::var("USBTRACE").is_ok() && (off < 0x70 || off == 0x100 || (0x400..0x444).contains(&off) || (0x500..0x600).contains(&off)) {
                eprintln!("USBWR off=0x{:x} val=0x{:x}", off, val & mask(size));
            }
            let v = (val & mask(size)) as u32;
            if off < 0x70 && off % 4 == 0 {
                let idx = (off / 4) as usize;
                let old = self.usb_glb[idx];
                match off {
                    0x00 => {
                        // GOTGCTL (QEMU: RO bits preserved both ways).
                        const RO: u32 = (0x1f << 22) | (1 << 20) | (1 << 19) | (1 << 18) | (1 << 17) | (1 << 16) | (1 << 8) | (1 << 0);
                        let mut w = (v & !RO) | (old & RO);
                        // SESREQ rising -> session success.
                        if ((w & (1 << 1)) != 0) && ((old & (1 << 1)) == 0) {
                            w |= 1 << 0; // SESREQSCS
                            self.usb_glb[0x04 / 4] |= 1 << 8; // SES_REQ_SUC
                            self.usb_raise_gint(1 << 2); // OTGINT
                            self.usb_sync_otgint();
                        } else if ((w & (1 << 1)) == 0) && ((old & (1 << 1)) != 0) {
                            w &= !(1 << 0);
                        }
                        // HNPREQ rising -> host negotiation detected.
                        if ((w & (1 << 9)) != 0) && ((old & (1 << 9)) == 0) {
                            w |= 1 << 8; // HSTNEGSCS
                            self.usb_glb[0x04 / 4] |= 1 << 17; // HST_NEG_DET
                            self.usb_raise_gint(1 << 2);
                            self.usb_sync_otgint();
                        } else if ((w & (1 << 9)) == 0) && ((old & (1 << 9)) != 0) {
                            w &= !(1 << 8);
                        }
                        self.usb_glb[idx] = w;
                    }
                    0x04 => {
                        // GOTGINT: W1C (facade parity: guest writes 1
                        // to clear; OTGINT drops when empty).
                        self.usb_glb[idx] &= !v;
                        self.usb_sync_otgint();
                    }
                    0x10 => {
                        // GRSTCTL: AHBIDLE forced, DMAREQ cleared,
                        // self-clearing bits latched-then-cleared.
                        let mut w = v | (1 << 31);
                        w &= !(1 << 30);
                        if (w & 1) != 0 || ((w >> 1) & 1) != 0 {
                            // CSFTRST/HSFTRST: QEMU reset_enter for
                            // the sticky core words (guest FIFO/HCFG
                            // programs survive, like QEMU's mmio
                            // arrays — only the protocol words reset).
                            self.usb_glb[0x00 / 4] = 0x000c0000 | (1 << 19) | (1 << 18) | (1 << 16);
                            self.usb_glb[0x04 / 4] = 0;
                            self.usb_glb[0x14 / 4] = (1 << 28) | (1 << 26) | (1 << 5) | (1 << 0);
                            self.usb_glb[0x18 / 4] = 0;
                            w &= !((1 << 1) | 1);
                        }
                        if ((w >> 4) & 1) != 0 {
                            self.usb_lower_gint(1 << 4); // RXFFLSH clears RXFLVL
                            w &= !(1 << 4);
                        }
                        if ((w >> 5) & 1) != 0 {
                            self.usb_raise_gint((1 << 5) | (1 << 26)); // TXFFLSH -> FIFOs empty
                            w &= !(1 << 5);
                        }
                        self.usb_glb[idx] = w;
                    }
                    0x14 => {
                        // GINTSTS: W1C (QEMU arm: val|=~old, val=~val,
                        // then RO bits re-set — net effect: written-1
                        // bits clear except read-only ones).
                        const RO: u32 = (1 << 26) | (1 << 25) | (1 << 24) | (1 << 19) | (1 << 18) | (1 << 7) | (1 << 6) | (1 << 5) | (1 << 4) | (1 << 2) | (1 << 0);
                        let mut cur = old;
                        cur &= !((v & !RO) & cur);
                        self.usb_glb[idx] = cur;
                        self.usb_sync_otgint();
                    }
                    _ => {
                        self.usb_glb[idx] = v;
                    }
                }
            } else if (0x400..0x444).contains(&off) && off % 4 == 0 {
                let idx = ((off - 0x400) / 4) as usize;
                match off {
                    0x440 => {
                        // HPRT0 (QEMU hreg0 arm verbatim, minus the
                        // usb_port_reset call — the LAN7800 is always
                        // attached, so PRTRST falling enables directly).
                        let old = self.usb_hreg0[idx];
                        let mut w = v;
                        w |= old & ((0x3 << 17) | (0x3 << 10) | (1 << 4) | (1 << 0));
                        w |= old & ((1 << 7) | (1 << 6));
                        if ((old & (1 << 2)) == 0) && ((w & (1 << 2)) != 0) {
                            w &= !(1 << 2);
                        }
                        let tmask = (1 << 5) | (1 << 3) | (1 << 2) | (1 << 1);
                        let tval = (!((w & tmask) | !((old & tmask) | !tmask))) & tmask;
                        w = (w & !tmask) | tval;
                        if ((w & (1 << 8)) == 0) && ((old & (1 << 8)) != 0) {
                            // PRTRST falling: port reset done -> ENA.
                            w |= (1 << 2) | (1 << 3); // ENA|ENACHG
                        }
                        if (w & ((1 << 5) | (1 << 3) | (1 << 1))) != 0 {
                            self.usb_raise_gint(1 << 24); // PRTINT
                        } else {
                            self.usb_lower_gint(1 << 24);
                        }
                        // PPWR set -> LAN7800 attach (CONNSTS|CONNDET).
                        if ((w & (1 << 12)) != 0) && !self.usb_hprt_conn {
                            self.usb_hprt_conn = true;
                            w |= (1 << 1) | (1 << 0);
                            self.usb_raise_gint(1 << 24);
                        }
                        self.usb_hreg0[idx] = w;
                    }
                    0x408 | 0x410 | 0x414 => {} // HFNUM/HPTXSTS/HAINT: read-only
                    _ => {
                        if off == 0x418 {
                            self.usb_hreg0[idx] = v & 0xffff; // HAINTMSK: 16 bits
                        } else {
                            self.usb_hreg0[idx] = v;
                        }
                    }
                }
            } else if (0x500..0x600).contains(&off) && off % 4 == 0 {
                let ch = ((off - 0x500) / 0x20) as usize;
                let reg = (((off - 0x500) % 0x20) / 4) as usize;
                if ch < 8 && reg < 8 {
                    match reg {
                        0 => {
                            // HCCHAR (QEMU hreg1 arm verbatim): CHDIS
                            // rising -> clear CHENA + CHHLTD; CHENA
                            // rising -> enable + usb_xfer.
                            let old = self.usb_hch[ch][0];
                            let mut w = v;
                            if ((w & (1 << 30)) != 0) && ((old & (1 << 30)) == 0) {
                                w &= !((1 << 31) | (1 << 30));
                                self.usb_hch[ch][2] |= 1 << 1; // CHHLTD
                                self.usb_hch[ch][0] = w;
                                // Re-walk the host IRQ (QEMU
                                // dwc2_update_hc_irq after disflg).
                                let masked = self.usb_hch[ch][2] & self.usb_hch[ch][3] & !(0x3ffff << 14);
                                if masked != 0 {
                                    self.usb_hreg0[(0x414 - 0x400) / 4] |= 1 << ch;
                                    let haint = self.usb_hreg0[(0x414 - 0x400) / 4];
                                    let haintmsk = self.usb_hreg0[(0x418 - 0x400) / 4] & 0xffff;
                                    if (haint & haintmsk) != 0 {
                                        self.usb_raise_gint(1 << 25);
                                    }
                                }
                            } else {
                                w |= old & (1 << 30);
                                if ((w & (1 << 31)) != 0) && ((old & (1 << 31)) == 0) {
                                    w &= !(1 << 30);
                                    self.usb_hch[ch][0] = w;
                                    self.usb_xfer(ch);
                                } else {
                                    w |= old & (1 << 31);
                                    self.usb_hch[ch][0] = w;
                                }
                            }
                        }
                        2 => {
                            // HCINT: W1C + reserved-bits mask (QEMU).
                            let old = self.usb_hch[ch][2];
                            let mut cur = old;
                            cur &= !((v & !(0x3ffff << 14)) & cur);
                            self.usb_hch[ch][2] = cur;
                        }
                        3 => {
                            // HCINTMSK: reserved bits stay 0 (QEMU).
                            self.usb_hch[ch][3] = v & !(0x3ffff << 14);
                        }
                        6 => {} // HCDMAB: read-only (QEMU logs + ignores)
                        _ => {
                            self.usb_hch[ch][reg] = v;
                        }
                    }
                }
            }
            return Ok(());
        }
        // DMA ch0 + ENABLE page: window backing with RMW (the facade
        // windows are RAM). No immediate action — the edge logic runs
        // in sync_in, mirroring the facade.
        if Self::is_dma(addr, size) {
            let base = if addr >= DMA_ENABLE_PAGE {
                DMA_ENABLE_PAGE
            } else {
                DMA_BASE
            };
            for i in 0..size {
                let o = addr - base + i;
                let idx = (o / 4) as usize;
                let sh = 8 * (o % 4);
                let b = ((val >> (8 * i)) & 0xff) as u32;
                if addr >= DMA_ENABLE_PAGE {
                    if let Some(cell) = self.dma_en_back.get_mut(idx) {
                        *cell = (*cell & !(0xff << sh)) | (b << sh);
                    }
                } else if let Some(cell) = self.dma_back.get_mut(idx) {
                    *cell = (*cell & !(0xff << sh)) | (b << sh);
                }
            }
            return Ok(());
        }
        // PWM (mirrors pwm.js): window backing (CTL/STA published at
        // sync_out); DAT1 pushes while USEF1 latched, FIFO pushes;
        // depth capped at 256 like the facade.
        if Self::is_pwm(addr, size) {
            let off = addr - PWM_BASE;
            let idx = (off / 4) as usize;
            if idx < self.pwm_back.len() {
                let base = (off % 4) as u32;
                let mut cell = self.pwm_back[idx];
                for i in 0..size {
                    let sh = 8 * (base + i as u32);
                    if sh < 32 {
                        cell &= !(0xff << sh);
                        cell |= (((val >> (8 * i)) & 0xff) as u32) << sh;
                    }
                }
                self.pwm_back[idx] = cell;
                if off == 0x14 {
                    // DAT1: push while USEF1 latched.
                    if (self.pwm_ctl & (1 << 5)) != 0 && self.pwm_fifo.len() < 256 {
                        self.pwm_fifo.push(cell);
                    }
                } else if off == 0x20 && self.pwm_fifo.len() < 256 {
                    self.pwm_fifo.push(cell);
                }
            }
            return Ok(());
        }
        // MMU_CTL compat: writing root|1 (or root|0) programs the real
        // regime; cells echo for the status poll + DONE park.
        if Self::is_mmuctl(addr, size) {
            let off = addr - MMU_CTL;
            let v = (val & mask(size)) as u32;
            if off == 0x00 {
                self.mmu_ctl_cell = v;
                if v & 1 != 0 {
                    self.mmu_ttbr0 = (v & !0xfff) as u64;
                    self.mmu_tcr = 16; // T0SZ=16: 48-bit VA, 4K granule
                    self.mmu_mair = 0xff;
                    self.mmu_sctlr |= 1;
                    self.mmu_loose = true;
                } else {
                    self.mmu_sctlr &= !1;
                    self.mmu_loose = false;
                }
            } else if off == 0x04 {
                self.mmu_done_cell = v;
            }
            return Ok(());
        }
        // SMP spin-table window: plain byte backing.
        if Self::is_smp(addr, size) {
            for i in 0..size {
                let o = addr - SMP_BASE + i;
                if (o as usize) < self.smp_mem.len() {
                    self.smp_mem[o as usize] = ((val >> (8 * i)) & 0xff) as u8;
                }
            }
            return Ok(());
        }
        if Self::is_page(addr, size, MBOX_PAGE) || Self::is_page(addr, size, LOCAL_BASE) {
            // GPU_ROUTING latch (LOCAL+0x0C): gates CORE_IRQ_SRC bit 8
            // (see the read arm). Timer/mailbox control cells (+0x40/
            // +0x50) latch (see the read arm). All other local cells
            // absorb.
            if Self::is_page(addr, size, LOCAL_BASE) && addr - LOCAL_BASE == 0x0c {
                self.local_gpu_routing = (val & mask(size)) as u32;
            }
            if Self::is_page(addr, size, LOCAL_BASE) && addr - LOCAL_BASE == 0x40 {
                self.local_timer_ctl0 = (val & mask(size)) as u32;
            }
            if Self::is_page(addr, size, LOCAL_BASE) && addr - LOCAL_BASE == 0x50 {
                self.local_mbox_ctl0 = (val & mask(size)) as u32;
            }
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
        // Mini-UART publish (mirrors uart1.js syncOut): ENABLES echo,
        // LSR TX-empty|idle once enabled, and the IO pulse-clear (the
        // guest's putc1 waits for the slot to clear).
        self.uart1_back[1] = if self.uart1_enabled { 1 } else { 0 };
        self.uart1_back[0x54 / 4] = if self.uart1_enabled {
            (1 << 5) | (1 << 6)
        } else {
            0
        };
        self.uart1_back[0x40 / 4] = 0;
        // I2C/SPI status publish (mirrors i2c.js/spi.js syncOut): the
        // guest only observes slice-boundary snapshots — never live
        // state (a mid-chunk DONE would let polls exit a chunk early
        // and shift all downstream timing by a constant phase).
        self.i2c_pub_c = (self.i2c_c & ((1 << 15) | 1)) | (if self.i2c_sdone { 1 << 7 } else { 0 });
        self.i2c_pub_s = if self.i2c_sdone { 1 << 7 } else { 0 };
        // M67 DWC2 SOF tick (facade syncOut parity): HFNUM advances a
        // frame per chunk while the core is touched/enabled; SOF bit
        // pulses every 8th tick. GINTSTS served live at read time, so
        // no cell publish is needed — only the counter moves here.
        self.usb_frame = self.usb_frame.wrapping_add(1) & 0x3fff;
        self.usb_sof_ticks = self.usb_sof_ticks.wrapping_add(1);
        if (self.usb_sof_ticks & 7) == 0 {
            self.usb_raise_gint(1 << 3); // SOF
        }
        {
            let mut cs = 0u32;
            if self.spi_ta {
                cs |= 1 << 7;
            }
            cs |= 1 << 18;
            if !self.spi_rx.is_empty() {
                cs |= 1 << 17;
            }
            if self.spi_sdone {
                cs |= 1 << 16;
            }
            self.spi_pub_cs = cs;
        }
        self.spi_cs_dirty = None;
        // Mailbox publish (mirrors syncMailboxOut): reply visible iff
        // a request was processed (idle STATUS bit31 FULL set, like the
        // old facade — the fb/shell guests poll bit31 at +0x04 and the
        // debug guest pins +0x18 idle 0 via the separate always-clear
        // read cell; do NOT fold the bits here).
        if self.mbx_pending {
            self.mbx_pub_status = 0;
            self.mbx_pub_read = self.mbx_addr;
        } else {
            self.mbx_pub_status = 0x80000000;
            self.mbx_pub_read = 0;
        }
        // PWM publish (mirrors pwm.js syncOut): STA from FIFO depth,
        // CTL canonical latch value.
        {
            let mut sta = 0u32;
            if self.pwm_fifo.len() >= 256 {
                sta |= 1 << 0;
            }
            if self.pwm_fifo.is_empty() {
                sta |= 1 << 1;
            }
            if self.pwm_back.len() > 1 {
                self.pwm_back[1] = sta;
            }
            if !self.pwm_back.is_empty() {
                self.pwm_back[0] = self.pwm_ctl;
            }
        }
        // DMA publish (mirrors syncDmaOut): CS shows END|INT only.
        let mut dcs = 0u32;
        if self.dma_end {
            dcs |= 2;
        }
        if self.dma_int {
            dcs |= 4;
        }
        if !self.dma_back.is_empty() {
            self.dma_back[0] = dcs;
        }
        self.dma_last_cs = dcs;
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
        // Mini-UART enable latch (mirrors uart1.js syncIn: sticky on any
        // nonzero ENABLES write; the publish happens in sync_out).
        if self.uart1_back[1] != 0 {
            self.uart1_enabled = true;
        }
        // PWM latch + drain (mirrors pwm.js syncIn): CTL levels latch on
        // change (CLRF1 edge clears the FIFO), then 64 samples drain.
        {
            let v = self.pwm_back[0];
            if v != self.pwm_last_ctl {
                self.pwm_last_ctl = v;
                self.pwm_ctl = v & ((1 << 0) | (1 << 1) | (1 << 5) | (1 << 7));
                if v & (1 << 6) != 0 {
                    self.pwm_fifo.clear();
                }
            }
            let take = core::cmp::min(self.pwm_fifo.len(), 64);
            self.pwm_ring.extend(self.pwm_fifo.drain(..take));
            self.pwm_drained += take as u64;
        }
        // DMA edge logic (mirrors main.js syncDmaIn, reading the window
        // backing like readU32: ABORT clears; ACTIVE rising (vs last
        // published) with a chain + enabled runs it now; a guest-cleared
        // INT unlatches (the same-slice ACTIVE write carries no INT bit,
        // so it can't wipe a fresh latch — only an explicit clear does).
        // ENABLE lives at +0x50 of the ENABLE page (index 20).
        let cs = self.dma_back[0];
        let conblk = self.dma_back[1];
        let enable = self.dma_en_back[(0x50 / 4) as usize];
        if cs & (1 << 31) != 0 {
            self.dma_end = false;
            self.dma_int = false;
        } else {
            if (cs & 1) != 0
                && (self.dma_last_cs & 1) == 0
                && conblk != 0
                && (enable & 1) != 0
            {
                let inten = self.dma_run_chain(conblk as u64);
                self.dma_end = true;
                if inten {
                    self.dma_int = true;
                }
            }
            if self.dma_int && (self.dma_last_cs & 4) != 0 && (cs & 4) == 0 {
                self.dma_int = false;
            }
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
        // M58 hot-path: MMU-off identity (goldens + kernel prologue).
        let pc = if (self.mmu_sctlr & 1) == 0 {
            pc
        } else {
            self.translate(pc)?
        };
        if self.in_ram(pc, 4) {
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
    /// EL2 drop state for the M53 kernel track (rust-raspberrypi-OS
    /// 09_privilege_level shape): SPSR_EL2/ELR_EL2 latched by MSR,
    /// SP_EL1 latched by MSR, current EL (2 at reset, 1 after an
    /// EL2->EL1 eret). HCR/CNTHCTL/CNTVOFF MSRs are absorbed (no trap
    /// model yet). Encodings from assembler truth (see M53 AGENTS entry):
    /// CurrentEL MRS=0xD5384240, MPIDR_EL1 MRS=0xD53800A1,
    /// CNTFRQ_EL0 MRS=0xD53BE002, SPSR_EL2=0xD53C4003/0xD51C4004,
    /// ELR_EL2=0xD53C4025/0xD51C4026, SP_EL1=0xD51C4107/0xD53C4108,
    /// HCR_EL2=0xD51C1109, CNTHCTL=0xD53CE10A/0xD51CE10B,
    /// CNTVOFF=0xD51CE06C, eret=0xD69F03E0.
    pub spsr_el2: u64,
    pub elr_el2: u64,
    pub sp_el1: u64,
    pub cur_el: u8,
    spsr_el2_msrd: bool,
    /// M57 thread registers: SP_EL0 + TPIDR_EL1 (per-CPU current).
    /// Backed u64s, zero at reset; MRS/MSR wired in the system arm.
    pub sp_el0: u64,
    pub tpidr_el1: u64,
    /// M69 exclusive monitor (single-core): LDXR/LDAXR records (addr,
    /// value, size); STXR/STLXR + CAS-family succeed only when the
    /// current memory matches the reservation (real exclusive
    /// semantics). Before M69 every STXR/CAS succeeded unconditionally
    /// (status 0), which broke TICKET-LOCK unlock: the unlock CASAL
    /// compared a STALE LDXR snapshot (always-0 because pi-cpu never
    /// recorded reservations) and overwrote the lock word, so the
    /// holder's refcount put never completed and the ...b92e44 waiter
    /// spun forever on w4=0x10000. With the monitor, the stale CAS
    /// fails (memory changed since the reservation) and the lock word
    /// survives — execution-proven by the w4 series collapsing to 0.
    excl_addr: u64,
    excl_val: u64,
    excl_size: u64,
    excl_valid: bool,
    /// M54 sync-exception state (ch11/12 shape): ESR_EL1/FAR_EL1
    /// filled on SVC entry; ESR MSR absorbed (MRS returns the latched
    /// syndrome). Encodings from assembler truth: ESR MRS=0xD5385200,
    /// MSR=0xD5185200; FAR MRS=0xD5386000; DAIF MRS=0xD53B4220 (MSR
    /// 0xD51B4220 writes the 4-bit field); ELR {3,0,4,0,1}
    /// MRS=0xD5384020/MSR=0xD5184020; SPSR {3,0,4,0,0}
    /// MRS=0xD5384000/MSR=0xD5184000; SVC=0xD4+imm16.
    pub esr_el1: u64,
    pub far_el1: u64,
    n: bool,
    z: bool,
    c: bool,
    v: bool,
    /// FPSR cumulative exception flags (bits 4:0 = IXC/UFC/OFC/DZC/IOC),
    /// set by the scalar-FP ALU below. FPCR is hardwired default (RN,
    /// no traps — the firmware never writes it, verified by disassembly:
    /// 12 MRS reads, 0 MSR writes). FPSR reads return these bits.
    fpsr_cum: u32,
}

impl Cpu {
    pub fn new(entry: u64) -> Self {
        Cpu { x: [0; 31], sp: 0, pc: entry, q: [0; 32], vbar_el1: 0, daif: 0xf, elr_el1: 0, spsr_el1: 0, spsr_el2: 0, elr_el2: 0, sp_el1: 0, cur_el: 2, spsr_el2_msrd: false, esr_el1: 0, far_el1: 0, sp_el0: 0, tpidr_el1: 0, excl_addr: 0, excl_val: 0, excl_size: 0, excl_valid: false, n: false, z: false, c: false, v: false, fpsr_cum: 0 }
    }

    /// M56 Linux-track reset: ARM64 boot protocol regs (x0=DTB PA,
    /// x1=x2=x3=0), MMU off, EL2, DAIF masked, SP seeded high.
    /// Call after `load_linux()` with `LINUX_DTB_PA`.
    pub fn linux_reset(&mut self, entry: u64, dtb_pa: u64, sp: u64) {
        self.x = [0; 31];
        self.x[0] = dtb_pa;
        self.sp = sp;
        self.pc = entry;
        self.cur_el = 2;
        self.daif = 0xf;
        self.vbar_el1 = 0;
        self.elr_el1 = 0;
        self.spsr_el1 = 0;
        self.spsr_el2 = 0;
        self.elr_el2 = 0;
        self.spsr_el2_msrd = false;
        self.esr_el1 = 0;
        self.far_el1 = 0;
        self.excl_valid = false;
        self.n = false;
        self.z = false;
        self.c = false;
        self.v = false;
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
    /// Scalar-FP access through the Q file low half (d31 is a real
    /// register — no XZR aliasing on the FP side). S writes
    /// zero-extend into the full 128 bits (architectural; unobservable
    /// while Q/vector forms fault).
    #[inline]
    fn fr(&self, r: u32) -> u64 {
        self.q[r as usize] as u64
    }
    #[inline]
    fn fw(&mut self, r: u32, v: u64, is64: bool) {
        self.q[r as usize] = if is64 { v as u128 } else { (v & 0xffff_ffff) as u128 };
    }
    /// Set an FPSR cumulative flag (0 IOC, 1 DZC, 2 OFC, 3 UFC, 4 IXC).
    #[inline]
    fn fpsr_set(&mut self, bit: u32) {
        self.fpsr_cum |= 1 << bit;
    }

    /// Finish a double binary op (host-computed r, RN like hardware):
    /// NaN propagation + OF/UF/IX. `invalid` = caller-detected Invalid
    /// Operation on non-NaN inputs (0/0, inf-inf, 0*inf); `dz` = divide-
    /// by-zero (inf result, DZ not OF); `exact` = caller-proven exactness
    /// (else IXC). Returns the result bits.
    fn fp_end_bin64(&mut self, ab: u64, bb: u64, r: f64, invalid: bool, dz: bool, exact: bool) -> u64 {
        let a = f64::from_bits(ab);
        let b = f64::from_bits(bb);
        // SNaN operand: IOC + quiet it preserving sign+payload
        // (fork-verified; pure-invalid (0/0 etc.) takes DefaultNaN).
        if fp_is_snan64(ab) {
            self.fpsr_set(0);
            return fp_quiet64(ab);
        }
        if fp_is_snan64(bb) {
            self.fpsr_set(0);
            return fp_quiet64(bb);
        }
        if invalid {
            self.fpsr_set(0);
            return 0x7ff8_0000_0000_0000;
        }
        if a.is_nan() || b.is_nan() {
            return r.to_bits(); // quiet passthrough (payload via host op)
        }
        if dz {
            self.fpsr_set(1);
            return r.to_bits();
        }
        if r.is_infinite() {
            if a.is_finite() && b.is_finite() {
                self.fpsr_set(2);
                self.fpsr_set(4);
            }
            return r.to_bits();
        }
        if r.abs() < f64::MIN_POSITIVE && (r != 0.0 || !exact) {
            self.fpsr_set(3);
            self.fpsr_set(4);
            return r.to_bits();
        }
        if !exact {
            self.fpsr_set(4);
        }
        r.to_bits()
    }

    /// Single-precision twin of fp_end_bin64.
    fn fp_end_bin32(&mut self, ab: u32, bb: u32, r: f32, invalid: bool, dz: bool, exact: bool) -> u32 {
        let a = f32::from_bits(ab);
        let b = f32::from_bits(bb);
        if fp_is_snan32(ab) {
            self.fpsr_set(0);
            return fp_quiet32(ab);
        }
        if fp_is_snan32(bb) {
            self.fpsr_set(0);
            return fp_quiet32(bb);
        }
        if invalid {
            self.fpsr_set(0);
            return 0x7fc0_0000;
        }
        if a.is_nan() || b.is_nan() {
            return r.to_bits();
        }
        if dz {
            self.fpsr_set(1);
            return r.to_bits();
        }
        if r.is_infinite() {
            if a.is_finite() && b.is_finite() {
                self.fpsr_set(2);
                self.fpsr_set(4);
            }
            return r.to_bits();
        }
        if r.abs() < f32::MIN_POSITIVE && (r != 0.0 || !exact) {
            self.fpsr_set(3);
            self.fpsr_set(4);
            return r.to_bits();
        }
        if !exact {
            self.fpsr_set(4);
        }
        r.to_bits()
    }

    /// Floating-point compare to NZCV (fcmp/fcmpe/fccmp-taken).
    /// Unordered (any NaN) always sets V (fork-verified — even quiet
    /// QNaN on a non-signaling compare); IOC only when signaling or
    /// an SNaN is involved.
    fn fp_cmp64(&mut self, ab: u64, bb: u64, signaling: bool) {
        let a = f64::from_bits(ab);
        let b = f64::from_bits(bb);
        let unord = a.is_nan() || b.is_nan();
        let snan = fp_is_snan64(ab) || fp_is_snan64(bb);
        if unord {
            if signaling || snan {
                self.fpsr_set(0);
            }
            self.set_flags(false, false, true, true);
        } else if a < b {
            self.set_flags(true, false, false, false);
        } else if a == b {
            self.set_flags(false, true, true, false);
        } else {
            self.set_flags(false, false, true, false);
        }
    }

    /// Single-precision twin of fp_cmp64 (V=1 on any unordered too).
    fn fp_cmp32(&mut self, ab: u32, bb: u32, signaling: bool) {
        let a = f32::from_bits(ab);
        let b = f32::from_bits(bb);
        let unord = a.is_nan() || b.is_nan();
        let snan = fp_is_snan32(ab) || fp_is_snan32(bb);
        if unord {
            if signaling || snan {
                self.fpsr_set(0);
            }
            self.set_flags(false, false, true, true);
        } else if a < b {
            self.set_flags(true, false, false, false);
        } else if a == b {
            self.set_flags(false, true, true, false);
        } else {
            self.set_flags(false, false, true, false);
        }
    }

    /// Float-to-int convert (FCVTZS/ZU/AS): `ibits` = 32/64 dest width,
    /// `mode` = fp_round_mode code. Saturates with IOC on overflow/NaN/Inf
    /// (NaN -> 0); IXC iff a finite in-range value was rounded.
    fn fp_to_int64(&mut self, av: u64, ibits: u32, unsigned: bool, mode: u32) -> u64 {
        let av = if mode == 4 && fp_is_subnormal64(av) {
            if av >> 63 == 1 {
                0x8000_0000_0000_0000
            } else {
                0
            }
        } else {
            av
        };
        let a = f64::from_bits(av);
        if a.is_nan() {
            self.fpsr_set(0);
            return 0;
        }
        let r = fp_round_mode(a, mode);
        // Overflow bounds (exact f64 constants).
        let (lo, hi) = if unsigned {
            if ibits == 64 {
                (0.0, 18446744073709551616.0)
            } else {
                (0.0, 4294967296.0)
            }
        } else if ibits == 64 {
            (-9223372036854775808.0, 9223372036854775808.0)
        } else {
            (-2147483648.0, 2147483648.0)
        };
        if r < lo || r >= hi || !r.is_finite() {
            self.fpsr_set(0);
            return if unsigned {
                if a < 0.0 { 0 } else if ibits == 64 { u64::MAX } else { 0xffff_ffff }
            } else if a < 0.0 {
                if ibits == 64 { i64::MIN as u64 } else { 0xffff_ffff_8000_0000 }
            } else if ibits == 64 {
                i64::MAX as u64
            } else {
                0xffff_ffff_7fff_ffff
            };
        }
        if r != a {
            self.fpsr_set(4);
        }
        // In range and integral: the cast is exact.
        if unsigned {
            r as u64
        } else {
            r as i64 as u64
        }
    }

    /// Single-precision twin of fp_to_int64.
    fn fp_to_int32(&mut self, av: u32, ibits: u32, unsigned: bool, mode: u32) -> u64 {
        let av = if mode == 4 && fp_is_subnormal32(av) {
            if av >> 31 == 1 {
                0x8000_0000
            } else {
                0
            }
        } else {
            av
        };
        let a = f32::from_bits(av);
        if a.is_nan() {
            self.fpsr_set(0);
            return 0;
        }
        let r = fp_round_mode(a as f64, mode);
        let (lo, hi) = if unsigned {
            if ibits == 64 {
                (0.0, 18446744073709551616.0)
            } else {
                (0.0, 4294967296.0)
            }
        } else if ibits == 64 {
            (-9223372036854775808.0, 9223372036854775808.0)
        } else {
            (-2147483648.0, 2147483648.0)
        };
        if r < lo || r >= hi || !r.is_finite() {
            self.fpsr_set(0);
            return if unsigned {
                if a < 0.0 { 0 } else if ibits == 64 { u64::MAX } else { 0xffff_ffff }
            } else if (a as f64) < 0.0 {
                if ibits == 64 { i64::MIN as u64 } else { 0xffff_ffff_8000_0000 }
            } else if ibits == 64 {
                i64::MAX as u64
            } else {
                0xffff_ffff_7fff_ffff
            };
        }
        if r != a as f64 {
            self.fpsr_set(4);
        }
        if unsigned {
            r as u64
        } else {
            r as i64 as u64
        }
    }

    /// Int-to-float convert (SCVTF/UCVTF). IXC iff the integer is not
    /// exactly representable. That is a SIGNIFICAND test, not a magnitude
    /// test: an integer is exact iff it is zero or its significant-bit
    /// count (bit-length minus trailing zeros) fits the mantissa (53 for
    /// double, 24 for single). The old `mag > 2^53/2^24` check over-fired
    /// IX on exactly-representable large values (e.g. 2^30 -> f32, 2^60
    /// -> f64) — proven against the oracle (fixed-point flag suite:
    /// oracle FPSR.IX=0 where we set it). Scaling by 2^fbits in the
    /// fixed-point rows is exponent-only (exact, no over/underflow in
    /// these ranges) and preserves the significand, so this single check
    /// is also the honest IX for the SCALED quotient — no separate
    /// scaled-exactness analysis needed.
    fn fp_from_int(&mut self, v: u64, ibits: u32, unsigned: bool, is64fp: bool) -> u64 {
        if is64fp {
            let r: f64 = match (ibits, unsigned) {
                (64, false) => v as i64 as f64,
                (64, true) => v as f64,
                (_, false) => (v as i32) as f64,
                _ => (v as u32) as f64,
            };
            let mag: u128 = match (ibits, unsigned) {
                (64, false) => (v as i64).unsigned_abs() as u128,
                (64, true) => v as u128,
                (_, false) => (v as i32).unsigned_abs() as u128,
                _ => (v as u32) as u128,
            };
            if r.is_finite() && fp_sig_bits(mag) > 53 {
                self.fpsr_set(4);
            }
            r.to_bits()
        } else {
            let r: f32 = match (ibits, unsigned) {
                (64, false) => v as i64 as f32,
                (64, true) => v as f32,
                (_, false) => (v as i32) as f32,
                _ => (v as u32) as f32,
            };
            let mag: u128 = match (ibits, unsigned) {
                (64, false) => (v as i64).unsigned_abs() as u128,
                (64, true) => v as u128,
                (_, false) => (v as i32).unsigned_abs() as u128,
                _ => (v as u32) as u128,
            };
            if r.is_finite() && fp_sig_bits(mag) > 24 {
                self.fpsr_set(4);
            }
            r.to_bits() as u64
        }
    }

    /// Round to integral (FRINT*): `mode` = fp_round_mode code, `set_ix`
    /// adds IXC when the value changed (FRINTX; the others never set it).
    fn fp_rint64(&mut self, av: u64, mode: u32, set_ix: bool) -> u64 {
        // Fork quirk (probed): away-mode flushes subnormal inputs to
        // signed zero (other modes coincide either way).
        let av = if mode == 4 && fp_is_subnormal64(av) {
            if av >> 63 == 1 {
                0x8000_0000_0000_0000
            } else {
                0
            }
        } else {
            av
        };
        let a = f64::from_bits(av);
        if a.is_nan() {
            if fp_is_snan64(av) {
                self.fpsr_set(0);
                return fp_quiet64(av);
            }
            return av;
        }
        if a.is_infinite() {
            return av;
        }
        let r = fp_round_mode(a, mode);
        if set_ix && r != a {
            self.fpsr_set(4);
        }
        r.to_bits()
    }

    /// Single-precision twin of fp_rint64.
    fn fp_rint32(&mut self, av: u32, mode: u32, set_ix: bool) -> u32 {
        let av = if mode == 4 && fp_is_subnormal32(av) {
            if av >> 31 == 1 {
                0x8000_0000
            } else {
                0
            }
        } else {
            av
        };
        let a = f32::from_bits(av);
        if a.is_nan() {
            if fp_is_snan32(av) {
                self.fpsr_set(0);
                return fp_quiet32(av);
            }
            return av;
        }
        if a.is_infinite() {
            return av;
        }
        let r = fp_round_mode(a as f64, mode) as f32;
        if set_ix && r != a {
            self.fpsr_set(4);
        }
        r.to_bits()
    }

    /// Narrow double to single (FCVT S,D) with OF/UF/IX.
    fn fp_narrow(&mut self, av: u64) -> u32 {
        let a = f64::from_bits(av);
        if a.is_nan() {
            if fp_is_snan64(av) {
                self.fpsr_set(0);
                return (f64::from_bits(fp_quiet64(av)) as f32).to_bits();
            }
            return (a as f32).to_bits();
        }
        let r = a as f32;
        if r.is_infinite() && a.is_finite() {
            self.fpsr_set(2);
            self.fpsr_set(4);
        } else if r == 0.0 {
            if a != 0.0 {
                self.fpsr_set(3);
                self.fpsr_set(4);
            }
        } else if r.abs() < f32::MIN_POSITIVE {
            self.fpsr_set(3);
            self.fpsr_set(4);
        } else if (r as f64) != a {
            self.fpsr_set(4);
        }
        r.to_bits()
    }

    /// Widen single to double (FCVT D,S): exact; SNaN raises IOC.
    fn fp_widen(&mut self, av: u32) -> u64 {
        if fp_is_snan32(av) {
            self.fpsr_set(0);
            return (f32::from_bits(fp_quiet32(av)) as f64).to_bits();
        }
        (f32::from_bits(av) as f64).to_bits()
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
        if self.cur_el == 2 && self.spsr_el2_msrd {
            // EL2->EL1 drop (M53 kernel track, 09_privilege_level shape):
            // consume the latched SPSR_EL2/ELR_EL2, mask DAIF like the
            // SPSR's I bit says, switch the stack to SP_EL1, report EL1.
            // cur_el==2 ONLY happens after an explicit MSR to an EL2
            // register (spsr_el2_msrd below): IRQ-entry guests resume at
            // EL1 with SPSR_EL1/ELR_EL1 (the pre-M53 path) and must NOT
            // take this branch (lirq's native eret died here: spsr_el2
            // reads 0 -> pc=0 -> Illegal(0) at 4117).
            let s = self.spsr_el2;
            self.n = (s >> 31) & 1 != 0;
            self.z = (s >> 30) & 1 != 0;
            self.c = (s >> 29) & 1 != 0;
            self.v = (s >> 28) & 1 != 0;
            let i = (s >> 7) & 1 != 0;
            if i {
                self.daif |= 0x2;
            } else {
                self.daif &= !0x2;
            }
            self.sp = self.sp_el1;
            self.pc = self.elr_el2;
            self.cur_el = 1;
            return;
        }
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
        // M63 write-watch pc tag: Bus::write owns the store but not the
        // guest pc. `wwatch_hits` counts watch log lines, so step emits
        // the pc tag ONLY on steps that actually hit (exact volume, no
        // flood — a `grep -A1 WWATCH` pairs each store with its author).
        let before = bus.wwatch_hits;
        let r = self.exec(bus, pc, w);
        if bus.wwatch_on
            && std::env::var("WWATCH").is_ok()
            && bus.wwatch_hits != before
        {
            eprintln!("WWPC pc=0x{:x}", pc);
        }
        r
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

// ---- scalar-FP (VFP) helpers: bit patterns in u64/u32, host IEEE ops
// (RN, like the hardware with default FPCR). NaN payloads propagate
// per the host (matches the fork on the probed vectors; the fuzzer
// carries NaN cases to catch drift). Cumulative FPSR flags are set by
// the methods below (bit numbers in Cpu::fpsr_set).

/// Expand an FMOV 8-bit float immediate (assembler+oracle ground truth:
/// sign=a, exp=(b?base_hi:base_lo)|(cdefgh>>4), frac=(cdefgh&15)<<off).
fn fmov_imm(imm8: u32, is64: bool) -> u64 {
    let a = (imm8 >> 7) & 1;
    let b = (imm8 >> 6) & 1;
    let c = imm8 & 0x3f;
    if is64 {
        let exp = (if b == 1 { 0x3fcu64 } else { 0x400 }) | ((c >> 4) as u64);
        ((a as u64) << 63) | (exp << 52) | (((c & 0xf) as u64) << 48)
    } else {
        let exp = (if b == 1 { 0x7cu32 } else { 0x80 }) | (c >> 4);
        (((a << 31) | (exp << 23) | ((c & 0xf) << 19)) & 0xffff_ffff) as u64
    }
}

#[inline]
fn fp_is_snan64(b: u64) -> bool {
    b & 0x7ff0_0000_0000_0000 == 0x7ff0_0000_0000_0000
        && b & 0x0008_0000_0000_0000 == 0
        && b & 0x0007_ffff_ffff_ffff != 0
}
#[inline]
fn fp_is_snan32(b: u32) -> bool {
    b & 0x7f80_0000 == 0x7f80_0000 && b & 0x0040_0000 == 0 && b & 0x003f_ffff != 0
}

/// Significant-bit count of a nonzero magnitude (bit-length minus
/// trailing zeros); zero has none. An integer is exactly representable
/// as an IEEE float iff this fits the mantissa (53 double / 24 single).
/// Used for honest IXC on int->float converts (oracle-proven: the old
/// magnitude test over-fired on exact powers like 2^30->f32).
#[inline]
fn fp_sig_bits(mag: u128) -> u32 {
    if mag == 0 {
        0
    } else {
        128 - mag.leading_zeros() - mag.trailing_zeros()
    }
}

/// Quiet a signaling NaN preserving sign+payload (fork-verified: the
/// fork quiets SNaNs this way rather than returning DefaultNaN).
#[inline]
fn fp_quiet64(b: u64) -> u64 {
    b | 0x0008_0000_0000_0000
}
/// Single-precision twin of fp_quiet64.
#[inline]
fn fp_quiet32(b: u32) -> u32 {
    b | 0x0040_0000
}

/// Subnormal test (for the away-mode flush below).
#[inline]
fn fp_is_subnormal64(b: u64) -> bool {
    b & 0x7ff0_0000_0000_0000 == 0 && b & 0x000f_ffff_ffff_ffff != 0
}
/// Single-precision twin of fp_is_subnormal64.
#[inline]
fn fp_is_subnormal32(b: u32) -> bool {
    b & 0x7f80_0000 == 0 && b & 0x007f_ffff != 0
}

/// Exactness of r = a+b / a-b (doubles) via integer arithmetic when
/// both operands are integral and small. Over-approximates inexact
/// (returns false when unsure) — IX may be set spuriously in razor
/// cases; nothing live reads IX (only newlib's dead helper).
fn fp_exact_addsub(a: f64, b: f64, r: f64, sub: bool) -> bool {
    if !a.is_finite() || !b.is_finite() || !r.is_finite() {
        return true; // overflow/invalid flagged separately
    }
    if a.fract() != 0.0 || b.fract() != 0.0 {
        return false;
    }
    const LIM: f64 = 9007199254740992.0; // 2^53
    if a.abs() > LIM || b.abs() > LIM {
        return false;
    }
    let (ai, bi) = (a as i128, b as i128);
    if (ai as f64) != a || (bi as f64) != b {
        return false;
    }
    let s = if sub { ai - bi } else { ai + bi };
    s.abs() <= (1i128 << 53) && (s as f64) == r
}

/// Exactness of r = a*b (doubles) via i128.
fn fp_exact_mul(a: f64, b: f64, r: f64) -> bool {
    if !a.is_finite() || !b.is_finite() || !r.is_finite() {
        return true;
    }
    if a.fract() != 0.0 || b.fract() != 0.0 {
        return false;
    }
    const LIM: f64 = 9007199254740992.0;
    if a.abs() > LIM || b.abs() > LIM {
        return false;
    }
    let (ai, bi) = (a as i64, b as i64);
    if (ai as f64) != a || (bi as f64) != b {
        return false;
    }
    match (ai as i128).checked_mul(bi as i128) {
        Some(p) => p.abs() <= (1i128 << 53) && (p as f64) == r,
        None => false,
    }
}

/// Round-to-integral with an explicit mode (frint*/fcvt* need several;
/// 0=z(trunc) 1=n(ties-even) 2=p(ceil) 3=m(floor) 4=a(away) 5=i(RN)).
fn fp_round_mode(x: f64, mode: u32) -> f64 {
    match mode {
        0 => x.trunc(),
        2 => x.ceil(),
        3 => x.floor(),
        4 => {
            let t = x.trunc();
            if t == x {
                t
            } else {
                t + x.signum()
            }
        }
        _ => {
            // ties-even (n and i/RN): manual, no toolchain dependency.
            // (f == 0.5 exactly needs |x| < 2^52, so the i64 cast below
            // is always in range on that path.)
            let t = x.trunc();
            let f = (x - t).abs();
            if f < 0.5 {
                t
            } else if f > 0.5 {
                t + x.signum()
            } else if (t as i64) & 1 == 0 {
                t
            } else {
                t + x.signum()
            }
        }
    }
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
        // BR / BLR / RET. M57 BLR FIX (kernel-proven): `br x8`=
        // 0xD61F0100 vs `blr x8`=0xD63F0100 differ ONLY in bit21.
        // The old code read opc=bits(22,21) as one field — for blr
        // that gives 0b01, which the match DID handle... except bit22
        // is also the discriminator the old match used for RET (0b10):
        // br-vs-blr collapsed correctly but the 0xD61F0100 word took
        // the 0b00 BR arm and dropped LR. Split: bit21==link (BLR),
        // bit22==RET-form. Assembler truth: br=D61F, blr=D63F,
        // ret x8=D65F0100, ret=D65F03C0 (handled by exact-word eret
        // above only for the bare ret; register rets land here).
        if ((w >> 25) & 0x7f) == 0b1101011 {
            let link = bits(w, 21, 21) == 1;
            let target = self.r(rn);
            if link {
                self.x[30] = self.pc;
            }
            self.pc = target;
            return Ok(());
        }
        // Exception-generating: SVC/HVC/BRK (M54 ch12 shape + M60 BRK).
        // SVC fills ESR_EL1 (EC=0x15, ISS=imm16) and takes the EL1h sync
        // vector synchronously: ELR=fault pc, SPSR=pstate(), DAIF masked,
        // pc=VBAR+0x200. HVC is absorbed (M58 PSCI probe); BRK is absorbed
        // (M60 WARN path). Anything else in 0xD4 (SMC/...): out of scope.
        // Encoding: SVC word=0xD4+imm16<<5+01
        // (low byte 0x01; HVC is 0x02, SMC 0x03, BRK is 0x00 — the old
        // (w&0xff)==1 test was right for the wrong reason, documented
        // here so it survives: `svc #0x1337`=0xD40266E1 ends 0xE1,
        // NOT 0x01).
        // M58 HVC-NOP (kernel PSCI probe `hvc #0`=0xD4000002 at the
        // 1.29M point): EL2 has no hypervisor under pi-cpu — PSCI calls
        // (CPU_ON/SUSPEND) have no second core to wake. Absorb as NOP
        // with x0 preserved (the kernel treats a zero return as
        // NOT_SUPPORTED and continues single-core; verified: boot
        // proceeds past the probe instead of faulting).
        if (w >> 24) == 0xD4 {
            if (w & 0b11) == 0b01 {
                let imm = ((w >> 5) & 0xffff) as u64;
                self.esr_el1 = (0x15 << 26) | imm;
                self.elr_el1 = pc;
                self.spsr_el1 = self.pstate();
                self.daif = 0xf;
                let vbar = if self.vbar_el1 == 0 { 0x100000 } else { self.vbar_el1 };
                self.pc = vbar + 0x200;
                return Ok(());
            }
            if (w & 0b11) == 0b10 {
                // HVC (see comment above): absorb as NOP, x0 preserved.
                return Ok(());
            }
            // BRK (M60 BRK-NOP, stepat-proven: `brk #0x800`=0xD4210000 at
            // the 25.8M point — the kernel's WARN path plants a BRK and
            // expects the EL1 debug vector to print + continue. No debug
            // model under pi-cpu: absorb as NOP so boot continues past
            // the warning instead of faulting the whole run).
            if (w & 0b11) == 0b00 {
                return Ok(());
            }
            return Err(ill);
        }
        // Hints (NOP/ISB/DMB/DSB/PAC/AUT/BTI/...): functional no-ops
        // in this model. PAC/AUT (pointer auth, e.g. 0xD50323BF AUTIASP
        // the kernel emits in every function epilogue) strip to NOP:
        // pi-cpu stores raw pointers (no auth codes). BTI (branch
        // target identification, e.g. 0xD503245F BTI C at every
        // indirect-call landing pad) is likewise a NOP: pi-cpu does
        // not enforce branch-target guards. WFI/WFE (0xD503207F /
        // 0xD503205F, assembler truth below) are NOPs that a
        // uniprocessor guest uses to idle: the kernel's completion
        // weighing loops spin on WFE with IRQs masked, and the reply is
        // already synchronous in this model — stalling the vCPU until
        // an (always-masked) IRQ would wedge the boot. Assembler truth:
        // autiasp=D50323BF autibsp=D50323FF paciasp=D503233F bti c=
        // D503245F wfi=D503207F wfe=D503205F (all (w>>12)==0xD5032,
        // already covered — comment documents why).
        if (w >> 12) == 0xD5032 || (w >> 12) == 0xD5033 {
            return Ok(());
        }
        // Cache maintenance (M60 DC-ZVA, zero-cost when masked): DC ZVA
        // {L=0,op0=1,op1=3,CRn=7,CRm=4,op2=1} word=0xD50B7420 zeroes
        // the 64-byte block at [Xt] (A53 DminLine=4 from CTR 0x84448004).
        // DC CVAC/CIVAC ({..,7,10/14,..}) + IC IVAU ({..,7,5,..}) are
        // clean/invalidate (no caches modeled: NOP). DCZID reads 0x4
        // (BS=4), so the kernel's computed block size always matches
        // this arm's 64 bytes — no divergence to police. Assembler
        // truth above (dc.s). Other {1,3,7,...} ops fault honestly.
        if (w & 0xFFF00000) == 0xD5000000 {
            let op0 = (w >> 19) & 3;
            let op1 = (w >> 16) & 7;
            let crn = (w >> 12) & 15;
            let crm = (w >> 8) & 15;
            let op2 = (w >> 5) & 7;
            if op0 == 1 && op1 == 3 && crn == 7 {
                if crm == 4 && op2 == 1 {
                    // DC ZVA: zero 64 bytes at [r(rt)].
                    let addr = self.r(w & 31);
                    for i in 0..16u64 {
                        bus.write(addr.wrapping_add(i * 4), 4, 0)
                            .map_err(|_| Fault::UnmappedData(addr))?;
                    }
                    return Ok(());
                }
                if (crm == 10 || crm == 14 || crm == 5) && op2 == 1 {
                    return Ok(()); // DC CVAC/CIVAC, IC IVAU: NOP
                }
            }
        }
        if ((w >> 25) & 0x7f) == 0b1101010 {
            // System: VBAR_EL1 + DAIF (IRQ delivery) + the MMU sysregs
            // (TTBR0/TCR/MAIR/SCTLR feed bus translation state).
            // Encodings from assembler truth (`msr vbar_el1, x0`=0xD518C000,
            // `msr daifclr,#2`=0xD50342FF, `msr ttbr0_el1, x0`=0xD5182000,
            // `msr tcr_el1, x1`=0xD5182041, `msr mair_el1, x2`=0xD518A202,
            // `msr sctlr_el1, x3`=0xD5181003, MRS set in enc-sys: ESR=
            // 0xD5385200 FAR=0xD5386000 ELR=0xD5384020 SPSR=0xD5384000
            // VBAR=0xD538C000 TTBR0=0xD5382000 SCTLR=0xD5381000 NZCV=
            // 0xD53B4200 DAIF=0xD53B4220):
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
                // TTBR0_EL1 {3,0,2,0,0} / TTBR1_EL1 {3,0,2,0,1} /
                // TCR_EL1 {3,0,2,0,2} / SCTLR_EL1 {3,0,1,0,0}: CRn picks
                // the register, op2 refines TTBR0 vs TTBR1 vs TCR (all
                // CRn=2). Encodings from assembler truth (M57):
                // TTBR1 MRS=0xD5382020/MSR=0xD5182020.
                let v = self.r(rd);
                if l == 0 {
                    if crn == 2 && op2 == 0 {
                        bus.mmu_ttbr0 = v;
                    } else if crn == 2 && op2 == 1 {
                        bus.mmu_ttbr1 = v;
                    } else if crn == 2 && op2 == 2 {
                        bus.mmu_tcr = v;
                    } else if crn == 1 && op2 == 0 {
                        bus.mmu_sctlr = v;
                    }
                } else if crn == 2 && op2 == 0 {
                    self.w(rd, bus.mmu_ttbr0, true);
                } else if crn == 2 && op2 == 1 {
                    let v = bus.mmu_ttbr1;
                    self.w(rd, v, true);
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
                // SPSR_EL1 {3,0,4,0,0} MRS=0xD5384000/MSR=0xD5184000 /
                // ELR_EL1 {3,0,4,0,1} MRS=0xD5384020/MSR=0xD5184020
                // (op2 picks). M54: MSR latches (the handler context
                // restore needs it); MRS returns the entry snapshot.
                if l == 0 {
                    let v = self.r(rd);
                    if op2 == 0 {
                        self.spsr_el1 = v;
                    } else {
                        self.elr_el1 = v;
                    }
                } else {
                    let v = if op2 == 0 { self.spsr_el1 } else { self.elr_el1 };
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 0 && crn == 4 && crm == 1 && (op2 == 0 || op2 == 1) {
                // SP_EL0 {3,0,4,1,0} MRS=0xD5384100/MSR=0xD5184100 /
                // SP_ELx {3,0,4,1,1}: thread stack pointers. M57: the
                // kernel's early EL1 code uses SP_EL0 as its thread
                // register (per-CPU current). Backed per-CPU field;
                // encodings from assembler truth (mrs x1,sp_el0=
                // 0xD5384101 — op2 carries the Rd low bit, gate on
                // crm==1, not the exact word).
                if l == 0 {
                    self.sp_el0 = self.r(rd);
                } else {
                    let v = self.sp_el0;
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 0 && crn == 13 && crm == 0 && op2 == 4 {
                // TPIDR_EL1 {3,0,13,0,4} MRS=0xD538D081/MSR=0xD518D081:
                // per-thread ID (percpu current pointer). Same backing
                // class as SP_EL0; assembler truth above.
                if l == 0 {
                    self.tpidr_el1 = self.r(rd);
                } else {
                    let v = self.tpidr_el1;
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 0 && crn == 5 && crm == 2 && op2 == 0 {
                // ESR_EL1 {3,0,5,2,0} MRS=0xD5385200/MSR=0xD5185200:
                // syndrome filled by SVC entry (MRS returns it, MSR
                // absorbs like hardware's syndrome write).
                if l == 0 {
                    self.esr_el1 = self.r(rd);
                } else {
                    let v = self.esr_el1;
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 0 && crn == 6 && crm == 0 && op2 == 0 {
                // FAR_EL1 {3,0,6,0,0} MRS=0xD5386000: fault address
                // (MRS only; MSR absorbed — data-abort slice fills it).
                if l != 0 {
                    let v = self.far_el1;
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 3 && crn == 4 && crm == 2 && op2 == 1 {
                // DAIF {3,3,4,2,1} MRS=0xD53B4220/MSR=0xD51B4220:
                // the 4-bit field as BITS[9:6] of the register (bit3=D
                // at bit9, bit2=A at bit8, bit1=I at bit7, bit0=F at
                // bit6 — assembler truth: `and w1,w2,#0x80` tests I,
                // `and w0,w24,#0x80` tests I; the old code read/wrote
                // bits[3:0], so MRS returned 0xF (I invisible at bit7)
                // and MSR stored garbage. M60: the spinlock slow path
                // (1e8fa0: mrs x2,daif + cbz-I) never saw I set and
                // fell into the BRK recursion instead of returning.
                if l != 0 {
                    self.w(rd, (self.daif as u64) << 6, true);
                } else {
                    self.daif = ((self.r(rd) >> 6) & 0xf) as u8;
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
            } else if op0 == 3 && op1 == 3 && crn == 4 && crm == 4 && (op2 == 0 || op2 == 1) {
                // FPCR {3,3,4,4,0} / FPSR {3,3,4,4,1}. FPCR reads 0
                // (default RN, no traps — the firmware has 12 MRS reads
                // and 0 MSR writes, verified by disassembly); FPSR reads
                // the cumulative flags the FP ALU maintains. MSR to
                // either is absorbed (nothing executes one).
                if l != 0 {
                    let v = if op2 == 0 { 0 } else { self.fpsr_cum as u64 };
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 3 && crn == 14 && crm == 0 && op2 == 1 {
                // CNTPCT_EL0 {3,3,14,0,1}: read-only counter.
                if l != 0 {
                    let v = bus.cntpct;
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 3 && crn == 14 && crm == 2 {
                // CNTP_TVAL {..,2,0} / CTL {..,2,1} / CVAL {..,2,2}.
                // Assembler truth: cntp_ctl=0xD53BE220/D51BE220,
                // tval=0xD53BE200/D51BE200, cval=0xD53BE240/D51BE240.
                if l == 0 {
                    let v = self.r(rd);
                    if op2 == 0 {
                        // TVAL: CVAL = counter + value (32-bit offset).
                        bus.cntp_cval = bus.cntpct.wrapping_add(v & 0xffff_ffff);
                        if std::env::var("PI3_TIMERTRACE").is_ok() {
                            eprintln!("TMRMSR CNTP_TVAL=0x{:x} cval=0x{:x} cntpct=0x{:x}", v & 0xffff_ffff, bus.cntp_cval, bus.cntpct);
                        }
                    } else if op2 == 1 {
                        // M62: store ALL THREE bits (ENABLE|IMASK|ISTATUS
                        // ignored-on-write per ARM ARM DDI0487
                        // CNTP_CTL_EL0: bits[2:0] = ISTATUS|IMASK|
                        // ENABLE; bit 2 is read-only status, bits 1:0
                        // are R/W). The old `(v & 1)` mask silently
                        // dropped the guest's IMASK=1 (bit 1), so the
                        // timer IRQ storm the guest tried to mask kept
                        // delivering 21k IRQs into the weighing window
                        // and starved the mailbox completion. Proven by
                        // timer-trace: the guest writes CTL=0x3 at the
                        // IMASK site (frame5 92dd90 orr #2) but the old
                        // mask stored 0x1.
                        bus.cntp_ctl = (v & 0x7) as u32;
                        if std::env::var("PI3_TIMERTRACE").is_ok() {
                            eprintln!("TMRMSR CNTP_CTL=0x{:x}", v & 0x7);
                        }
                    } else if op2 == 2 {
                        bus.cntp_cval = v;
                        if std::env::var("PI3_TIMERTRACE").is_ok() {
                            eprintln!("TMRMSR CNTP_CVAL=0x{:x} cntpct=0x{:x}", v, bus.cntpct);
                        }
                    }
                } else if op2 == 1 {
                    // CTL read: ENABLE bit + ISTATUS (counter >= cval).
                    // M62: IMASK (bit 1) reads back from the stored
                    // value (see the MSR arm above); ISTATUS (bit 2) is
                    // computed live. Old code computed ENABLE|ISTATUS
                    // only, so a guest-set IMASK read back clear.
                    let mut c = bus.cntp_ctl & 0x3;
                    if bus.cntpct >= bus.cntp_cval {
                        c |= 1 << 2;
                    }
                    self.w(rd, c as u64, true);
                }
            } else if op0 == 3 && op1 == 3 && crn == 14 && crm == 3 {
                // CNTV_TVAL/CTLCVAL {..,3,0/1/2} + CNTVCT {..,0,2}: the
                // virtual timer, INDEPENDENT backing (M60 — see the Bus
                // field comment). CNTVCT reads the same physical counter
                // (no virtual offset on bare metal); CNTV MSRs never
                // touch CNTP state. Pre-fix CNTV_TVAL/CTLCVAL fell to
                // the catch-all (MSR absorbed, MRS read 0), so the
                // kernel's CNTV program silently vanished — harmless
                // then, but the unified {..,14,..} arm would have
                // aliased them onto CNTP.
                if l == 0 {
                    let v = self.r(rd);
                    if op2 == 0 {
                        bus.cntv_cval = bus.cntpct.wrapping_add(v & 0xffff_ffff);
                        if std::env::var("PI3_TIMERTRACE").is_ok() {
                            eprintln!("TMRMSR CNTV_TVAL=0x{:x} cval=0x{:x}", v & 0xffff_ffff, bus.cntv_cval);
                        }
                    } else if op2 == 1 {
                        bus.cntv_ctl = (v & 1) as u32;
                        if std::env::var("PI3_TIMERTRACE").is_ok() {
                            eprintln!("TMRMSR CNTV_CTL=0x{:x}", v & 1);
                        }
                    } else if op2 == 2 {
                        bus.cntv_cval = v;
                        if std::env::var("PI3_TIMERTRACE").is_ok() {
                            eprintln!("TMRMSR CNTV_CVAL=0x{:x}", v);
                        }
                    }
                } else if op2 == 1 {
                    let mut c = bus.cntv_ctl & 1;
                    if bus.cntpct >= bus.cntv_cval {
                        c |= 1 << 2;
                    }
                    self.w(rd, c as u64, true);
                }
            } else if op0 == 3 && op1 == 3 && crn == 14 && crm == 0 && op2 == 2 {
                // CNTVCT_EL0 {3,3,14,0,2} MRS=0xD53BE042 (assembler truth
                // from the 92de38 tick-setup row): same counter as
                // CNTPCT (no CNTVOFF on bare metal).
                if l != 0 {
                    let v = bus.cntpct;
                    self.w(rd, v, true);
                }
            } else if l == 0 && op1 == 3 && crn == 4 && (op2 & 0b110) == 0b110 {
                // DAIFSet/Clr: imm (CRm, 4 bits D/A/I/F) sets/clears the
                // named bits (op2 bit0: 1=clr, 0=set).
                if (op2 & 1) == 1 {
                    self.daif &= !(crm as u8 & 0xf);
                } else {
                    self.daif |= crm as u8 & 0xf;
                }
            } else if op0 == 3 && op1 == 3 && crn == 0 && crm == 0 && op2 == 1 {
                // CTR_EL0 {3,3,0,0,1} MRS=0xD53B0021 (assembler truth,
                // `mrs x1,ctr_el0`): Cortex-A53 cache-type 0x84448004
                // (QEMU aarch64_a53_initfn `cpu->ctr`; L1Ip=VIPT,
                // DminLine=4/IminLine=4 = 64-byte lines, matching the
                // kernel's cache-line-size probe at 0xffffffc0080270b0).
                // Pre-fix the MRS fell to the catch-all read-0, so the
                // probe's `cbnz memword` path stalled the boot at a
                // size-0 kmalloc (verified: faulting pc consumes the
                // 0x10-minimum allocation's NULL return).
                if l != 0 {
                    self.w(rd, 0x8444_8004, true);
                }
            } else if op0 == 3 && op1 == 0 && crn == 4 && crm == 2 && op2 == 2 {
                // CurrentEL {3,0,4,2,2} MRS=0xD5384240: report the EL.
                // (NOT {3,0,0,0,2} — first cut used a hand-derived
                // encoding and the kernel parked at 0x8004C forever;
                // CRn=4/CRm=2 straight from the objdump above.)
                if l != 0 {
                    self.w(rd, (self.cur_el as u64) << 2, true);
                }
            } else if op0 == 3 && op1 == 1 && crn == 0 && crm == 0 && op2 == 1 {
                // CLIDR_EL1 {3,1,0,0,1} MRS=0xD5390021 (assembler truth,
                // `mrs x2,clidr_el1`): A53-like 3-level cache topology
                // (LoUIS=2/LoUU=2/LoC=2: L1D+L1I+L2, no L3+). The
                // kernel's cpuinfo probe (0xffffffc0080227d4) tests
                // CLIDR[27:21] for L3+ presence; 0 takes the L1/L2
                // path. Cache-type fields (Ctype*, LoC/LoU*) all read
                // 0: the kernel's cache-maintenance-by-level loop
                // (flush_cache_all at 0xffffffc008022934+) terminates
                // immediately instead of wandering the levels — proven
                // by execution (0x0A200023 faulted at the loop's LDXR
                // at +142.5M). Pre-fix the MRS fell to the catch-all
                // read-0; this arm pins the same 0 explicitly so the
                // behavior survives future honest-value attempts.
                if l != 0 {
                    self.w(rd, 0, true);
                }
            } else if op0 == 3 && op1 == 3 && crn == 0 && crm == 0 && op2 == 7 {
                // DCZID_EL0 {3,3,0,0,7} MRS=0xD53B00E0 (assembler truth,
                // `mrs x0,dczid_el0`): DZP=0 (DC ZVA not prohibited),
                // BS=4 (64-byte blocks). Stored to cpuinfo+776.
                // BISECT-PROVEN: 0x4 is NOT the regression (0 still
                // faults at 142M; 0x4 reaches 150M). Restored.
                if l != 0 {
                    self.w(rd, 0x4, true);
                }
            } else if op0 == 3 && op1 == 0 && crn == 0 && crm == 0 && op2 == 5 {
                // MPIDR_EL1 {3,0,0,0,5} MRS=0xD53800A1: single core 0,
                // like the old facade unicorn core (affinity 0).
                if l != 0 {
                    self.w(rd, 0, true);
                }
            } else if op0 == 3 && op1 == 0 && crn == 0 && crm == 0 && (op2 == 0 || op2 == 6) {
                // MIDR_EL1 {3,0,0,0,0} MRS=0xD5380000 / REVIDR_EL1
                // {3,0,0,0,6} MRS=0xD53800C0 (idtrace: cpu_feature cable
                // at 0xffffffc0080227f0 reads MIDR/REVIDR/ID_AA64* in a
                // row): Cortex-A53 part (0x410FD034: ARM, A53, r0p4).
                // REVIDR reads 0 (no revision errata).
                if l != 0 {
                    let v = if op2 == 0 { 0x410FD034u64 } else { 0 };
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 0 && crn == 0 && crm == 4 && (op2 == 0 || op2 == 1 || op2 == 4 || op2 == 5) {
                // ID_AA64PFR0/1 {3,0,0,4,0/1} + ID_AA64ZFR0 {3,0,0,4,4} +
                // ID_AA64SMFR0 {3,0,0,4,5} (idtrace: head.S + cpufeature
                // probe at 0xffffffc00802284c): report a v8.0 A53 with
                // NO SVE/SME (0x0) so the kernel takes the scalar paths
                // (rdvl would fault — absorbed separately if reached).
                if l != 0 {
                    self.w(rd, 0, true);
                }
            } else if op0 == 3 && op1 == 0 && crn == 0 && crm == 5 && (op2 == 0 || op2 == 1) {
                // ID_AA64DFR0/1 {3,0,0,5,0/1} (idtrace: head.S + the
                // 0xffffffc00802280c row): no debug extensions.
                if l != 0 {
                    self.w(rd, 0, true);
                }
            } else if op0 == 3 && op1 == 0 && crn == 0 && crm == 6 && (op2 == 0 || op2 == 1 || op2 == 2) {
                // ID_AA64ISAR0/1/2 {3,0,0,6,0/1/2} (idtrace row): scalar
                // v8.0 ISAR (no atomics-LSE bits needed — the decoder
                // executes them anyway; features here only steer kernel
                // code paths, and 0 keeps it on baseline).
                if l != 0 {
                    self.w(rd, 0, true);
                }
            } else if op0 == 3 && op1 == 0 && crn == 0 && crm == 7 && (op2 == 0 || op2 == 1 || op2 == 2) {
                // ID_AA64MMFR0/1/2 {3,0,0,7,0/1/2} (idtrace: head.S +
                // cpufeature rows at 0xdb17b4/0xdb1850/0xffffffc008022834):
                // report 4K granule + 39-bit PARange (the walk's actual
                // shape: T0SZ=25/T1SZ=25, 4K only). MMFR0: PARange=001
                // (40-bit, closest honest to our 39) in bits[3:0].
                if l != 0 {
                    let v = if op2 == 0 { 0x1u64 } else { 0 };
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 3 && crn == 14 && crm == 0 && op2 == 0 {
                // CNTFRQ_EL0 {3,3,14,0,0} MRS=0xD53BE002: the real Pi 3
                // arch-timer rate (19.2 MHz). The 09 boot.s parks when
                // this reads 0; pi-cpu always reports the hardware rate.
                if l != 0 {
                    self.w(rd, 19_200_000, true);
                }
            } else if op0 == 3 && op1 == 4 && crn == 4 && crm == 0 && (op2 == 0 || op2 == 1) {
                // SPSR_EL2 {3,4,4,0,0} / ELR_EL2 {3,4,4,0,1} (op2 picks,
                // MRS=0xD53C4003/0xD53C4025, MSR=0xD51C4004/0xD51C4026):
                // latched by MSR, consumed by the EL2->EL1 eret.
                if l == 0 {
                    let v = self.r(rd);
                    if op2 == 0 {
                        self.spsr_el2 = v;
                        // Gate for the EL2->EL1 eret branch: only an
                        // explicit MSR SPSR_EL2 arms it (IRQ-entry guests
                        // resumewith SPSR_EL1/ELR_EL1 and must not drop).
                        self.spsr_el2_msrd = true;
                    } else {
                        self.elr_el2 = v;
                    }
                } else if op2 == 0 {
                    let v = self.spsr_el2;
                    self.w(rd, v, true);
                } else {
                    let v = self.elr_el2;
                    self.w(rd, v, true);
                }
            } else if op0 == 3 && op1 == 4 && crn == 4 && crm == 1 && op2 == 0 {
                // SP_EL1 {3,4,4,1,0} (MSR=0xD51C4107, MRS=0xD53C4108):
                // the EL1 stack the eret drop installs.
                if l == 0 {
                    self.sp_el1 = self.r(rd);
                } else {
                    let v = self.sp_el1;
                    self.w(rd, v, true);
                }
            } else if op1 == 4 && ((crn == 1 && crm == 1 && op2 == 1)
                || (crn == 14 && crm == 1 && (op2 == 1 || op2 == 3))
                || (crn == 14 && crm == 0 && op2 == 3))
            {
                // EL2 timer/trap setup with no trap model yet (absorbed):
                // HCR_EL2 {3,4,1,1,1} MSR=0xD51C1109, CNTHCTL_EL2
                // {3,4,14,1,1}: MRS 0xD53CE11F/MSR 0xD51CE11F (Rd field
                // differs MRS xzr=11111 vs MSR — match by fields, not
                // word), CNTVOFF_EL2 {3,4,14,0,3}: rustc emits
                // `msr cntvoff_el2,xzr`=0xD51CE07F (Rd=11111 leaks into
                // the CRm/op2 field — op2 reads 3 either way, gate on
                // the register not the exact word).
                // MRS reads return 0.
                if l != 0 {
                    self.w(rd, 0, true);
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
            // Load-vs-store is bit22 (REVERTED M57 bit30 experiment —
            // full 14-word assembler survey: STP=0xA9000440/LDP=
            // 0xA9400440 differ ONLY in bit22; bit30 is 0 for every
            // plain pair form. The kernel word 0xA9841D07 is genuinely
            // STP-pre-index (mode 0b11, offset +64): the OLD bit22 code
            // executed it as STP all along — CORRECT. The memmove
            // corruption came from elsewhere (still open); the bit30
            // swap broke 7 goldens (gpio/smp/irq/sd/rpi-kernel/
            // firmware/debug) by flipping every plain LDP into a
            // store. Lesson stands: pairs need the full survey, and
            // one-sided evidence (one kernel word) never flips a
            // golden-pinned gate.
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
                    // Post-index (mode 01): writeback FIRST (pre-access),
                    // fuzzer-pinned (mem_28_2 golden: fork keeps wb on
                    // fault). The 25.8M x21-zeroing is then a GENUINE
                    // faulting-pair side effect (or a different bug) —
                    // NOT evidence for wb-after-access. Revert to the
                    // golden-pinned ordering; re-examine 25.8M fresh.
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
                    if let Some(b) = wb {
                        self.wsp(rn, b, true);
                    }
                    self.w(rd, sext(v1, 32), true);
                    self.w(rt2, sext(v2, 32), true);
                } else {
                    let v1 = bus.read(addr, sc).map_err(|_| Fault::UnmappedData(addr))?;
                    let v2 = bus
                        .read(addr.wrapping_add(sc), sc)
                        .map_err(|_| Fault::UnmappedData(addr))?;
                    if let Some(b) = wb {
                        self.wsp(rn, b, true);
                    }
                    self.w(rd, v1, is64);
                    self.w(rt2, v2, is64);
                }
            } else {
                bus.write(addr, sc, if is64 { self.r(rd) } else { self.r(rd) & 0xffff_ffff })
                    .map_err(|_| Fault::UnmappedData(addr))?;
                let a2 = addr.wrapping_add(sc);
                bus.write(a2, sc, if is64 { self.r(rt2) } else { self.r(rt2) & 0xffff_ffff })
                    .map_err(|_| Fault::UnmappedData(a2))?;
                if let Some(b) = wb {
                    self.wsp(rn, b, true);
                }
            }
            return Ok(());
        }
        // SIMD STP/LDP S/D/Q: class bits(29:25) == 10110 (vs 10100
        // for integer pairs). Element size from bits(31:30): 00=S(4B),
        // 01=D(8B), 10=Q(16B). Scalar halves ride the Q file low bits
        // (zero-extended). Modes mirror integer pairs (00/10 offset,
        // 11 pre-index, 01 post-index) with the writeback timing the
        // fork shows (pre applies after success, post immediately).
        // (Was: Q-only — S/D pairs executed as 16-byte Q with x16
        // offsets, silently corrupting FP prologues.)
        if ((w >> 25) & 0x1f) == 0x16 {
            let esz: u64 = match bits(w, 31, 30) {
                0b00 => 4,
                0b01 => 8,
                0b10 => 16,
                _ => return Err(ill),
            };
            let is_load = bits(w, 22, 22) == 1;
            let mode = bits(w, 24, 23);
            let off = (sext(bits(w, 21, 15) as u64, 7) as i64).wrapping_mul(esz as i64) as u64;
            let rt2 = bits(w, 14, 10);
            let base = self.rsp(rn);
            let (addr, wb) = match mode {
                0b10 | 0b00 => (base.wrapping_add(off), None),
                0b11 => {
                    let a = base.wrapping_add(off);
                    (a, Some(a))
                }
                // Post-index (mode 01): writeback FIRST (pre-access), like
                // integer pairs (fuzzer-pinned: the fork keeps post-index
                // wb on fault).
                _ => {
                    let a = base;
                    self.wsp(rn, base.wrapping_add(off), true);
                    (a, None)
                }
            };
            if is_load {
                if esz == 16 {
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
                    let v1 = bus.read(addr, esz).map_err(|_| Fault::UnmappedData(addr))?;
                    let v2 = bus
                        .read(addr.wrapping_add(esz), esz)
                        .map_err(|_| Fault::UnmappedData(addr))?;
                    self.fw(rd, v1, esz == 8);
                    self.fw(rt2, v2, esz == 8);
                }
            } else if esz == 16 {
                let (l1, h1) = (self.q[rd as usize] as u64, (self.q[rd as usize] >> 64) as u64);
                let (l2, h2) = (self.q[rt2 as usize] as u64, (self.q[rt2 as usize] >> 64) as u64);
                bus.write(addr, 8, l1).map_err(|_| Fault::UnmappedData(addr))?;
                bus.write(addr.wrapping_add(8), 8, h1).map_err(|_| Fault::UnmappedData(addr))?;
                bus.write(addr.wrapping_add(16), 8, l2).map_err(|_| Fault::UnmappedData(addr))?;
                bus.write(addr.wrapping_add(24), 8, h2).map_err(|_| Fault::UnmappedData(addr))?;
            } else {
                let m = if esz == 8 { u64::MAX } else { 0xffff_ffff };
                bus.write(addr, esz, self.fr(rd) & m).map_err(|_| Fault::UnmappedData(addr))?;
                let a2 = addr.wrapping_add(esz);
                bus.write(a2, esz, self.fr(rt2) & m).map_err(|_| Fault::UnmappedData(a2))?;
                if let Some(b) = wb {
                    self.wsp(rn, b, true);
                }
            }
            return Ok(());
        }

        // SIMD STR/LDR S/D/Q (unsigned-imm/pre/post/unscaled). Class
        // bits(29:25) == 11110 (vs 11100 integer); element size from
        // bits(31:30): 10=S(4B), 11=D(8B), 00=Q(16B, the firmware's
        // NEON spills — kept working). Scalar halves ride the Q file
        // low bits. opc bit22: 0 store / 1 load (bit23 must be 0 for
        // scalars; 1x is unallocated). Register-offset mirrors the
        // integer form (extend_reg, amount = log2(esz)).
        if ((w >> 25) & 0x1f) == 0x1e {
            let opc = bits(w, 23, 22);
            match bits(w, 31, 30) {
                0b00 => {
                    if bits(w, 23, 23) != 1 {
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

                0b01 => {
                    // STR/LDR H (2B, float16 lanes — f_mkdir's dir-entry
                    // date word uses STUR H; oracle-verified like S/D).
                    // opc must be 00 (store) / 01 (load); 1x is
                    // unallocated. Loads zero-extend (fw convention).
                    if opc != 0b00 && opc != 0b01 {
                        return Err(ill);
                    }
                    let is_load = opc == 0b01;
                    let (addr, wb) = if bits(w, 24, 24) == 1 {
                        let off = (bits(w, 21, 10) as u64) * 2;
                        (self.rsp(rn).wrapping_add(off), None)
                    } else if bits(w, 21, 21) == 1 {
                        // Register offset (mirror of the integer form).
                        let rm = bits(w, 20, 16);
                        let option = bits(w, 15, 13);
                        let s = bits(w, 12, 12);
                        let amount = if s == 1 { 1 } else { 0 };
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
                                let off = sext(bits(w, 20, 12) as u64, 9);
                                let a = self.rsp(rn).wrapping_add(off);
                                (a, Some(a))
                            }
                            _ => return Err(ill),
                        }
                    };
                    if is_load {
                        let v = bus.read(addr, 2).map_err(|_| Fault::UnmappedData(addr))?;
                        self.fw(rd, v, false);
                    } else {
                        bus.write(addr, 2, self.fr(rd) & 0xffff)
                            .map_err(|_| Fault::UnmappedData(addr))?;
                    }
                    if let Some(b) = wb {
                        self.wsp(rn, b, true);
                    }
                    return Ok(());
                }
                0b10 | 0b11 => {
                    // STR/LDR S (10, 4B) / D (11, 8B). opc must be 00
                    // (store) / 01 (load); 1x is unallocated.
                    let esz: u64 = if bits(w, 31, 31) == 1 { 8 } else { 4 };
                    if opc != 0b00 && opc != 0b01 {
                        return Err(ill);
                    }
                    let is_load = opc == 0b01;
                    let (addr, wb) = if bits(w, 24, 24) == 1 {
                        let off = (bits(w, 21, 10) as u64) * esz;
                        (self.rsp(rn).wrapping_add(off), None)
                    } else if bits(w, 21, 21) == 1 {
                        // Register offset (mirror of the integer form).
                        let rm = bits(w, 20, 16);
                        let option = bits(w, 15, 13);
                        let s = bits(w, 12, 12);
                        let amount = if s == 1 { esz.trailing_zeros() } else { 0 };
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
                                let off = sext(bits(w, 20, 12) as u64, 9);
                                let a = self.rsp(rn).wrapping_add(off);
                                (a, Some(a))
                            }
                            _ => return Err(ill),
                        }
                    };
                    if is_load {
                        let v = bus.read(addr, esz).map_err(|_| Fault::UnmappedData(addr))?;
                        self.fw(rd, v, esz == 8);
                    } else {
                        let m = if esz == 8 { u64::MAX } else { 0xffff_ffff };
                        bus.write(addr, esz, self.fr(rd) & m)
                            .map_err(|_| Fault::UnmappedData(addr))?;
                    }
                    if let Some(b) = wb {
                        self.wsp(rn, b, true);
                    }
                    return Ok(());
                }
                _ => return Err(ill),
            }
        }

        // STR/LDR (unsigned imm, unscaled, pre/post-index, register offset)
        // M54: bit26==0/size==1 (LDRH/STRH, e.g. `ldrh w11,[x8,#8]`=
        // 0x7940110B) is the INTEGER halfword form, not SIMD — the old
        // gate faulted it as Illegal. Only bit26==1 diverts to SIMD.
        // M60 LDR-UIMM12 (class bits[29:25]==0b00100 — all of STXR/
        // STLXR(0x88/0xC8), CAS-family, STADD-one-byte(0xC8), LDRB/H/
        // STRB/H + plain LDR/STR) UNCONDITIONALLY FIRST, before the
        // 0x1c arm AND the atomic lane. Assembler truth (atom.s +
        // excl.s, both `.arch armv8.1-a+lse`): bit29 is the U bit — a
        // plain class test CANNOT split loads from exclusives.
        // Truth table (bits[29:25], all B/H/W/X forms checked):
        //   00100 + L==0 + o1==0 + o2==000 -> STXR/STLXR (store, +status)
        //   00100 + L==1 + o1==0           -> LDXR/LDAXR (load; Rs=31)
        //   00100 + o2==100/110 + Rs==31   -> STLR/LDAR (Rs-31 forms)
        //   00100 + o1==1                  -> CAS-family (store/RMW)
        //   00100 + L==0 + o2==000 + STATUS-FREE -> LDRB/H/W/X-U12 LOAD
        //   11100 + bit24==1               -> LDR/STR-U12 (plain)
        // so the discriminator is NOT a class test: inside class
        // 00100/L==0/o1==0/o2==000 the STXR shape and the plain
        // U12 load are bit-identical except Rs/Rt (spin Rs=2/Rt=1 vs
        // STXR Rs=4/Rt=5 — same o0/L/o1/op14:12 AND same Rs-bit18 in
        // general, e.g. C8037E62-stxr has Rs=3/bit18=0 just like the
        // spin). STATUS-FREE = Rs==Rn&&Rt==Rn misses too (spin
        // Rs=2/Rn=19 differs; stxr w4,w5,[x0] Rn=0 misses both ways).
        // THE disassembler settles it: 0x88027E61 = `stxr w2,w1,[x19]`
        // and 0xC803FE62 = `stlxr w3,x2,[x19]` — BOTH ARE STORES, never
        // loads. The "spin loads [x19,#636]" story (and its Rs-bit18
        // gate) was a forced-off probe artifact: disabling the arm
        // faults BEFORE the branch reads, so x1 NEVER LOADs 0x9376B40
        // — that value is the STALE x1 the comparison loop then spins
        // on, not a loaded one. Execute both as STXR: [x19]=x1/x2,
        // status 0 into Rs. The equal-compare then advances past the
        // loop instead of faulting or mis-loading. (Kills bit18-gated
        // "CAS-fires-LDR" bug too: 88A17C62-cas has Rs=1/bit18=0 and
        // took the LOAD path — writing Rd with [Rn] garbage.)
        if ((w >> 25) & 0x1f) == 0b00100 && bits(w, 22, 22) == 0 && bits(w, 21, 21) == 0 && bits(w, 23, 21) == 0b000 {
            // M60 WIDTH (same fix as LDXR above): W=4B, X=8B by
            // bits[31:30], never sf (sf==1 for both W-stxr 88027E61
            // and X-stxr C8037E62).
            // M69 MONITOR: STXR/STLXR succeed only when memory still
            // matches the LDXR reservation (addr+value+size); else
            // status 1 and NO store (real exclusive semantics — the
            // unconditional-success shortcut broke ticket-lock
            // unlock, see the excl_* field comment).
            let nbytes = 1u64 << bits(w, 31, 30);
            let addr = self.rsp(rn);
            let ok = if self.excl_valid && self.excl_addr == addr && self.excl_size == nbytes {
                bus.read(addr, nbytes).map(|cur| cur == (self.excl_val & mask(nbytes))).unwrap_or(false)
            } else {
                false
            };
            // Any STXR clears the reservation (success or fail — ARM ARM).
            self.excl_valid = false;
            let rs = bits(w, 20, 16);
            if ok {
                let v = if nbytes == 8 { self.r(rd) } else { self.r(rd) & mask(nbytes) };
                bus.write(addr, nbytes, v).map_err(|_| Fault::UnmappedData(addr))?;
                if rs != 31 {
                    self.w(rs, 0, false);
                }
            } else if rs != 31 {
                self.w(rs, 1, false);
            }
            return Ok(());
        }
        // Load-literal (M57 Linux-track): LDR Xt,[PC,#imm19*4] = 0x58,
        // LDR Wt = 0x18, LDRSW Xt = 0x98 (opc in bits31:30, Rt=rd).
        // The kernel's early code is full of literal pools (capability
        // tables, constants); without this every one faults Illegal.
        // PRFM-literal (0xD8+) is a prefetch hint — absorb as NOP.
        // Assembler truth: `ldr x8,=<addr>`=0x580000C8 (imm19=6).
        // Class test is bits[29:25]==0b01100 (opc=V=bit30 varies; a
        // bits[31:26] test MISSES the LDR-Xt form 0x58=0b010110).
        if ((w >> 25) & 0x1f) == 0b01100
        {
                let opc = bits(w, 31, 30);
                if opc == 0b11 {
                    return Ok(()); // PRFM literal: prefetch hint, NOP
                }
                let imm = sext(((bits(w, 23, 5)) as u64) << 2, 21);
                let addr = pc.wrapping_add(imm);
                match opc {
                    0b00 => {
                        // LDR Wt literal: 4-byte zero-extending load.
                        let v = bus.read(addr, 4).map_err(|_| Fault::UnmappedData(addr))?;
                        self.w(rd, v, false);
                    }
                    0b01 => {
                        // LDR Xt literal: 8-byte load.
                        let v = bus.read(addr, 8).map_err(|_| Fault::UnmappedData(addr))?;
                        self.w(rd, v, true);
                    }
                    _ => {
                        // LDRSW Xt literal: 4-byte SIGNED load to 64 bits.
                        let v = bus.read(addr, 4).map_err(|_| Fault::UnmappedData(addr))?;
                        self.w(rd, sext(v, 32), true);
                    }
                }
                return Ok(());
            }

        if ((w >> 25) & 0x1f) == 0x1c
            // M60 LSE-GUARD (assembler truth, atom.s): the register-offset
            // path (bit24==0, bit21==1) collides with the LSE atomic lane
            // (class 111000 = stadd/ldadd/swp/cas-ldapr shapes, all
            // bit24==0/bit21==1). Real discriminator (ARM ARM): integer
            // reg-offset fixes bits[11:10]==10 (B8616801/B8617801/
            // F82A780C all 10); LSE carries 00 (every LSE word: stadd,
            // swp, ldadd, ldapr...). Non-10 words are NOT integer
            // reg-offset — skip the whole arm so they reach the atomic
            // lane (previously stadd executed as a wrong-address
            // register-offset store and "passed" one.rs fault-only
            // checks while corrupting memory).
            && !(bits(w, 24, 24) == 0
                && bits(w, 21, 21) == 1
                && (((bits(w, 11, 11) << 1) | bits(w, 10, 10)) != 0b10))
            // M60 EXCLUSIVE-GUARD (bh.s assembler truth: B/H exclusives
            // share bits[29:25]==00100 with NO plain form — plain LDRB/H
            // is class 11100 (ldrb 0x39400001), exclusives are class
            // 00100 at every size (stlrb 0x089FFC01 ... stxr 0x88037E62).
            // So inside this arm, o1==0 + op==111 marks EXCLUSIVE at all
            // sizes (STXR/STLXR o2==000, LDXR/LDAXR o2==010/L==1/Rs==31,
            // STLR/LDAR o2==100/110/Rs==31); plain U12 never has op==111
            // with o1==0 here. Skip the arm for those (let the atomic
            // lane below own every exclusive shape). Class gate added
            // after the B9427E61 regression (plain `ldr w1,[x19,#636]`
            // class 11100 carries o1==0/op==111 too — without the class
            // test the guard stole ALL such loads into Err(ill)).
            && !(((w >> 25) & 0x1f) == 0b00100
                && bits(w, 21, 21) == 0
                && bits(w, 14, 12) == 0b111)
        {
            let size = bits(w, 31, 30);
            if bits(w, 26, 26) == 1 {
                return Err(ill); // SIMD (non-Q handled above)
            }
            let nbytes = 1u64 << size;
            let opc = bits(w, 23, 22);
            // PRFM (M57 register + M60 register-offset forms): every PRFM
            // is a prefetch hint — absorb as NOP, whatever the addressing
            // mode. Register form `prfm pstl1keep,[x17]`=0xF9800071
            // (size=3/opc=0b10/bit24==1/imm12==0); register-OFFSET form
            // `prfm pstl1keep,[x26,x0]`=0xF8A06B50 (bit24==0/bit21==1/
            // b11_10==10, the integer-reg-offset shape — stepat-proven at
            // the 25.6M point: opc==0b10/size==3 faulted as PRFM-imm
            // before reaching any offset decode). Gate on size==3/opc==
            // 0b10 ONLY (both forms); real LDRSW-X (signed 64-bit load)
            // shares opc==0b10/size==3 but is UNREACHABLE here — LDRSW-X
            // literal lives in the 0b01100 arm above and LDRSW-X reg-off
            // ... is distinguished by L==1 (loads) vs PRFM's L==0? NO —
            // prfm has no L bit. Truth: PRFM-register/offset are the ONLY
            // size==3/opc==0b10 words with bit24==0-or-(bit24==1&imm12==
            // 0); LDRSW-X forms carry a real offset. The kernel emits no
            // LDRSW-X-reg forms on this path (882 fuzzer + 23 smoke pin
            // the scalar forms), so absorb all size==3/opc==0b10 here.
            if size == 3 && opc == 0b10 {
                return Ok(()); // PRFM (all forms) / LDRSW-X-imm (absorbed)
            }
            // opc: 00 store, 01 zero-extending load, 10 signed load to
            // 64 bits (LDRSB/H/SW-X), 11 signed load to 32 bits (size<2;
            // unallocated for size>=2). PRFM-imm (size 3, opc 2) is
            // absorbed above (all PRFM forms are NOPs).
            let is_load = opc != 0b00;
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
                    // register offset (M57 LSL-FIX, kernel-proven): the
                    // amount is bit12 S selecting the NATURAL shift —
                    // LSL #esz-log2 for option==0b011 (plain LSL form).
                    // Assembler truth: `ldr x1,[x0,x1]`=0xF8616801
                    // (S=0, LSL #0) vs `ldr x1,[x0,x1,lsl#3]`=
                    // 0xF8617801 (S=1, LSL #3). The old code passed S
                    // itself as amount, so `str x12,[x0,x10,lsl#3]`
                    // shifted by 1 (pairwise aliasing, walked idx
                    // zero). Guests never use scaled register offsets
                    // (fuzzer green either way), so no golden moves.
                    // NOTE size==access-size: the kernel's W-form
                    // `ldr w1,[x0,x1]` (size=0b10, S=1) shifts by 2
                    // (esz=4), NOT 3 — amount=size is log2(esz) by
                    // construction (size 0/1/2/3 -> 1/2/4/8 bytes).
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

        // Atomics (M57 Linux-track, single-core model): everything the
        // kernel's early boot uses executes with acquire/release folded
        // (no SMP observers yet). Class gate: bits[29:24]==0b001000
        // (exclusive family: 0xC8/0x88 LDAXR/STLXR/LDAR/STLR/CAS) PLUS
        // the B/H-exclusive forms (stlrb 0x089FFC01 / stxrb / ldxrb /
        // ldarb — fullspin-proven: NO separate class exists. Recomputed
        // by hand: 0x089FFC01 >> 24 = 0x08, & 0x3f = 0b001000 — the SAME
        // gate as W/X. The "0001xx" comment was arithmetic error
        // (0x08 IS 0b001000, not 0b000100). B/H exclusives reach this
        // lane fine; what faulted them was the 0x1c-arm EXCLUSIVE-GUARD
        // below testing the same wrong constants — fixed with it.)
        // OR the LSE lane = bits[29:24]==0b111000
        // (M60 CORRECTED: NO Rm==0 gate — the old Rs==0 test belonged
        // to three 0x41-niche words, NOT the lane. Real LSE words carry
        // Rs=1/2/4/5/31: stadd B821027F, ldaddal B8E50062, LDAPR
        // B8BFC261/F8BFC261. atom.s proves it.)
        // PLACEMENT IS THE DISCRIMINATOR:
        // this arm sits AFTER the integer STR/LDR (0x1c) arm... EXCEPT
        // the LSE lane (bits[29:24]==0b111000) must ALSO survive the
        // 0x1c arm: the 0x1c test is bits[29:25]==11100 which MATCHES
        // 111000 (bit24==0), so LSE words with bit24==0 + bit21==0
        // (stadd B821027F, ldadd B8210062, swp B8218062, cas 88A17C62
        // — all bit24==0/bit21==0-or-1...) reach the 0x1c arm FIRST.
        // M60 PROOF this is safe: the 0x1c arm's (bit11,bit10) switch
        // rejects them — stadd's b11_10==00 takes the UNSCALED path
        // with imm9 = bits[20:12] = Rs+op = garbage offset, executing
        // a wrong-address RMW instead of faulting. Words that DO reach
        // the lane (bit24==1: B8E50062-ldaddal, B8BFC261-LDAPR,
        // B861027F-staddl...) prove the lane works; the bit24==0
        // siblings die in 0x1c first. Fix: gate the 0x1c arm to
        // bit24==1 (plain U12/imm9 forms only)... (see edit below).
        // (including the 253 guest post-index/reg-off words that share
        // Rm==0) — only words the 0x1c arm rejects reach here. The old
        // gate (top8 0x08/0x18, placed early) missed the entire 0xC8
        // family, so LDAXR/STLXR/CAS faulted Illegal (e.g. 0xC85FFC60
        // at the 1.15M-fault point). Truth: cas=C8A07C41 casa=C8E07C41
        // casl=C8A0FC41 casal=C8E0FC41 swp=F8208041 ldadd=F8200041
        // stlr=C89FFC20 ldar=C8DFFC20 ldaxr=C85FFC20 stlxr=C802FC20
        // casp=48207C82. Field map: o0=bit15 (0=exclusive family,
        // 1=LSE lane), L=bit22, Rs=bits[20:16].
        if ((w >> 24) & 0x3f) == 0b001000 || ((w >> 24) & 0x3f) == 0b111000
        {
            let o0 = bits(w, 15, 15);
            let l = bits(w, 22, 22);
            let o1 = bits(w, 21, 21);
            // M60 o2-FIX (assembler truth, excl.s — the M57 comment
            // below was WRONG: o2 is bits[23:21], a 3-bit field, and
            // bit20 is Rs's LOW BIT on stores / part of Rs==31 on
            // loads — never an opcode bit): o2 MUST be bits(w,23,21).
            // With o2=bit20 the o0==0/o1==0 family arm's o2==1 gates
            // matched bit20==1 words (LDXR/LDAXR/STLR/LDAR, whose Rs=31
            // sets bit20) and MISSED real o2==010/100/110 — so ldxr/
            // stlr/ldar faulted and CAS (bit20==0) fell to Err(ill).
            let o2 = bits(w, 23, 21);
            // Rs is bits[20:16] (status dest on STXR/STLXR stores,
            // comparand on CAS, 31 on LDXR/LDAXR/STLR/LDAR).
            let rs = bits(w, 20, 16) & 0x1f;
            // Exclusive family: LDXR/LDAXR (o2==010/L==1 loads) vs
            // STLR/LDAR (o2==100/110) vs STADD-one-byte (hoisted
            // below) vs CAS-family (o1==1, dedicated arm below).
            // LDAPR-proper 0xB8BFC261 faults honestly until a real one
            // is observed — no golden executes it. (The M58 LDAPR arm
            // that stood here is deleted: it executed the spin word as
            // a wrong-base load; the spin word is STXR per the
            // disassembler and runs in the STXR arm above.)
            // M57 o2==000 SHAPE (0xC8047C62, kernel-proven): STXR/
            // STLXR (o2==000, handled by the STXR arm above) and the
            // STADD-one-byte hoist below (op==111/L==0/o1==0) share
            // this lane; loads (LDXR/LDAXR o2==010) are gated
            // explicitly below.
            // M60 STXR-FIRST (disassembler-proven): the o2==000 stores
            // (STXR/STLXR) NEVER reach this lane — the unconditional
            // STXR arm above consumes class-00100/L==0/o1==0/o2==000
            // first (all widths, both o0 values). What remains here
            // with L==0 is STADD-one-byte (0xC8 lane) + CAS-family
            // (o1==1) + LDAPR (LSE lane, o0==1 gated below) — so the
            // op14:12==0b111 + L==0 hoist below is STADD, full stop
            // (no rd==31 gate needed: every word arriving here with
            // that shape IS an addend-store; STXR twins are gone).
            // M57 STADD-ARM ORDER (0xC803FE62 has o0==1): the STADD
            // check must run BEFORE the o0==0/o1==0 family gate, so it
            // is hoisted out here (matches both o0 values).
            // M60 LDAXR-EXEMPT (stlcheck-proven): LDAXR/LDAR (L==1)
            // must NEVER take this store hoist — ldaxr-w0 0x885FFE60
            // carries op==111/o1==0/L==1 and RMW-ADDed [x19]+=x0,
            // corrupting the ticket lock (mem 0x103 -> 0x9387308).
            // STADD-one-byte is L==0-only, so gate on l==0 (already)
            // AND require the o2==000 store shape... o2 is 010 here,
            // so the clean gate is o2==000. L==1 loads fall through
            // to the family arm regardless of op bits.
            if bits(w, 14, 12) == 0b111 && o1 == 0 && l == 0 && o2 == 0b000 {
                // One-byte STADD (disassembler: 0xC8047C62 =
                // `stxr w4,x2,[x3]`, 0xC803FE62 = `stlxr w3,x2,[x19]`;
                // addend is the Rm field bits[20:16] — Rs=4/Rt=2 and
                // Rs=3/Rt=2 respectively — NOT Rd. Plain RMW-ADD, no
                // status, no write-back.
                // M60 WIDTH (same fix): W=4B, X=8B by bits[31:30].
                let addr = self.rsp(rn);
                let size_b: u64 = 1u64 << bits(w, 31, 30);
                let old = bus.read(addr, size_b).map_err(|_| Fault::UnmappedData(addr))?;
                let v = self.r(rs);
                let res = old.wrapping_add(v & mask(size_b)) & mask(size_b);
                bus.write(addr, size_b, res).map_err(|_| Fault::UnmappedData(addr))?;
                return Ok(());
            }
            // Exclusive family (M60 assembler truth, excl.s — field map
            // CORRECTED: o2 is bits[23:21], NOT bit20 — the M57 comment
            // claiming "bit20 doubles as o2" was wrong and mis-gated
            // every load/store below):
            //   o2==010 + L==1 + Rs==31 -> LDXR/LDAXR (load; o0=acquire)
            //   o2==010 + L==1 + Rs!=31 -> (unallocated; fault honestly)
            //   o2==100 + L==0 + Rs==31 -> STLR (o0==1 always)
            //   o2==110 + L==1 + Rs==31 -> LDAR (o0==1 always)
            //   o2==101/111 + o1==1     -> CAS-family (store/RMW)
            //   o2==000 + L==0 + o1==0  -> STXR/STLXR (handled above)
            // STADD-one-byte (0xC8 lane, op==111/L==0/o1==0) is hoisted
            // above. LDAPR-proper 0xB8BFC261 faults honestly until a
            // real one is observed — no golden executes it. (The M58
            // LDAPR arm that stood here is deleted: it executed the
            // spin word as a wrong-base load; the spin word is STXR
            // per the disassembler and runs in the STXR arm above.)
            // M60 o0-DROP (bh.s: stlrb 0x089FFC01 has o0==1, ldxrb
            // 0x085F7E62 has o0==0 — o0 is acquire-hint-only on loads
            // AND stores in this family; the `o0==0` gate rejected every
            // STLR/LDAR/STLRB/STLRH/LDAR-form into Err(ill)).
            if o1 == 0 {
                let addr = self.rsp(rn);
                // M60 ACQUIRE-LOAD WIDTH (fullspin-proven): LDAXR is NOT
                // gated on o0 — ldaxr-w0 0x885FFE60 has o0==1, ldxr-w1
                // 0x885F7E61 has o0==0, same o2==010/L==1. The old
                // `o0==0` gate rejected LDAXR into Err(ill)... which the
                // kernel never survived to report because the window
                // trace ran first. BOTH o0 values load here; o0 is only
                // the acquire hint (folded, single-core).
                if o2 == 0b010 && l == 1 {
                    // Load: LDXR/LDAXR (acquire by o0). Rs==31 ALWAYS
                    // (disassembler: ldxr/ldaxr w2/x2 all Rs=31).
                    // M60 WIDTH FIX (spin-proven): size is bits[31:30]
                    // DIRECTLY (W=4B zero-extended, X=8B) — NOT sf. sf
                    // is bit31 ALONE (0xC8/0x88 both sf=1 for W AND X),
                    // so `if sf {8} else {4}` read 8 bytes for every
                    // W-load and smeared the neighbor dword into Rd —
                    // the wfe-spin's ldxr-w1 then never equaled sxtw-x0
                    // and the kernel parked at 2M forever.
                    if rs != 31 {
                        return Err(ill);
                    }
                    let nbytes = 1u64 << bits(w, 31, 30);
                    let v = bus.read(addr, nbytes).map_err(|_| Fault::UnmappedData(addr))?;
                    // M69 MONITOR: record the reservation (addr+value
                    // +size) for the STXR/CAS success check below.
                    self.excl_addr = addr;
                    self.excl_val = v & mask(nbytes);
                    self.excl_size = nbytes;
                    self.excl_valid = true;
                    self.w(rd, v, nbytes == 8);
                } else if (o2 == 0b100 && l == 0) || (o2 == 0b110 && l == 1) {
                    // STLR (o2==100/L==0) / LDAR (o2==110/L==1):
                    // Rs==31 always (stlrb/ldarb/h + W/X forms agree).
                    // M60 WIDTH (same fix as LDXR above): W=4B, X=8B by
                    // bits[31:30], never sf. (o0 gate dropped with the
                    // LDAXR fix above: STLR/LDAR always carry o0==1 per
                    // excl.s, but gating on it adds nothing — o2+L+Rs
                    // already discriminate.)
                    if rs != 31 {
                        return Err(ill);
                    }
                    if l == 1 {
                        let nbytes = 1u64 << bits(w, 31, 30);
                        let v = bus.read(addr, nbytes).map_err(|_| Fault::UnmappedData(addr))?;
                        self.w(rd, v, nbytes == 8);
                    } else {
                        let nbytes = 1u64 << bits(w, 31, 30);
                        let v = if nbytes == 8 { self.r(rd) } else { self.r(rd) & 0xffff_ffff };
                        bus.write(addr, nbytes, v).map_err(|_| Fault::UnmappedData(addr))?;
                    }
                } else if o2 == 0b101 || o2 == 0b111 {
                    // CAS-family (o1==1 guaranteed by the o0==0/o1==0
                    // gate... NO — this arm's gate is o1==0. CAS carries
                    // o1==1, so CAS never reaches here; it needs the
                    // dedicated arm below. Fault honestly.
                    return Err(ill);
                } else {
                    // o2==000 (STXR/STLXR: handled above) or anything
                    // else: UNREACHABLE — fault honestly so the next
                    // word names itself instead of executing wrong.
                    return Err(ill);
                }
                return Ok(());
            }
            // M60 STXP (stxp-probe: 0xC8200C9A `stxp w0,x26,x3,[x4]` at
            // the 25.8M point — exclusive PAIR store, o2==001/L==0/
            // o1==1/size==11 (X): plain 2×size store of Rt+Rt2 + status
            // 0 into Rs (single-core: always succeeds). MUST precede the
            // CASP arm (same o1==1/o2==001 shape class; Rt2!=31 + Rs==0
            // discriminates: CASP carries comparand pairs Rs/Rs2).
            // Truth: stxp-w 0x88230022 (size=10, Rs=3/Rt=2/Rt2=0),
            // stxp-x 0xC8230022, KERN 0xC8200C9A (Rs=0/Rt=26/Rt2=3/Rn=4).
            // Gate: Rt2!=31 (CASP's Rt2 field reads 31: casp 0x48227C80
            // has Rs=2/Rt=0/Rt2=31 — a COMPARAND pair, not a store pair;
            // STXP always carries a real second data reg). STXP with
            // Rs!=0 is NORMAL: stxp-w carries Rs=3 status, only KERN used
            // Rs=0. The old rs==0 gate rejected every non-KERN STXP.
            if o1 == 1 && o2 == 0b001 && l == 0 {
                let rt2 = bits(w, 14, 10);
                if rt2 != 31 {
                    let nbytes = 1u64 << bits(w, 31, 30);
                    let addr = self.rsp(rn);
                    let v1 = if nbytes == 8 { self.r(rd) } else { self.r(rd) & 0xffff_ffff };
                    let v2 = if nbytes == 8 { self.r(rt2) } else { self.r(rt2) & 0xffff_ffff };
                    bus.write(addr, nbytes, v1).map_err(|_| Fault::UnmappedData(addr))?;
                    bus.write(addr.wrapping_add(nbytes), nbytes, v2).map_err(|_| Fault::UnmappedData(addr))?;
                    self.w(rs, 0, false);
                    return Ok(());
                }
            }
            // CAS-family (M60 assembler truth, excl.s): o2==101/111 +
            // o1==1 (any o0/L: cas/casa/casl/casal + B/H/W/X). Rs =
            // comparand, Rt = new value; Rd=old always. Single-core:
            // plain compare-and-swap.
            // M60 WIDTH (same fix as LDXR above): W=4B, X=8B by
            // bits[31:30], never sf (sf==1 for both W-cas 88A17C62
            // and X-cas C8A17C62, so sf read 8B for W and smeared
            // the neighbor dword into Rt).
            // M60 LDXP (ldxp-probe: 0xc87f0022 `ldxp x2,x0,[x1]` at the
            // 100M fault point — exclusive PAIR load, o2==011/L==1/
            // o1==1/Rs==31/Rt2==Rt's pair): plain 2×size load into Rt
            // +Rt2 (single-core: no exclusivity tracking). MUST precede
            // the CAS arm (same o1==1/o2==111 shape; Rs==31 + Rt2!=31
            // discriminates: CAS carries a real comparand Rs).
            if o1 == 1 && o2 == 0b011 && l == 1 && rs == 31 {
                let nbytes = 1u64 << bits(w, 31, 30);
                let rt2 = bits(w, 14, 10);
                let addr = self.rsp(rn);
                let v1 = bus.read(addr, nbytes).map_err(|_| Fault::UnmappedData(addr))?;
                let v2 = bus.read(addr.wrapping_add(nbytes), nbytes).map_err(|_| Fault::UnmappedData(addr))?;
                self.w(rd, v1, nbytes == 8);
                self.w(rt2, v2, nbytes == 8);
                return Ok(());
            }
            if o1 == 1 && (o2 == 0b101 || o2 == 0b111) {
                let nbytes = 1u64 << bits(w, 31, 30);
                let size_b = nbytes;
                let addr = self.rsp(rn);
                let old = bus.read(addr, size_b).map_err(|_| Fault::UnmappedData(addr))?;
                let cmp = if size_b == 8 { self.r(rs) } else { self.r(rs) & 0xffff_ffff };
                let new = if size_b == 8 { self.r(rd) } else { self.r(rd) & 0xffff_ffff };
                // CAS semantics: if mem==cmp, mem=new; Rt=old always.
                // (M60 CORRECTED: the old code wrote Rd=old — but on
                // the CAS lane Rd-field IS Rt (comparand is Rs): cas
                // w1,w2,[x3]=0x88A17C62 has Rs=1/Rt=2 — old value goes
                // to Rt=w2, NOT to a separate Rd. Verified against the
                // disassembler field map, same as STXR above.)
                // M69 MONITOR: a CAS only stores when the memory also
                // matches the LDXR reservation (stale-comparand unlock
                // CASAL must fail once the lock word moved on — see the
                // excl_* field comment). The Rt=old writeback happens
                // regardless (like hardware's failed-CAS old return).
                let reserved_ok = if self.excl_valid && self.excl_addr == addr && self.excl_size == size_b {
                    old == (self.excl_val & mask(size_b))
                } else {
                    // No reservation (plain CAS without LDXR, e.g. the
                    // 0xC8A07C41 plain-CAS word): compare-only, no
                    // monitor gate — matches the old behavior exactly.
                    true
                };
                self.excl_valid = false;
                if old == (cmp & mask(size_b)) && reserved_ok {
                    bus.write(addr, size_b, new & mask(size_b))
                        .map_err(|_| Fault::UnmappedData(addr))?;
                }
                self.w(rd, old, size_b == 8);
                return Ok(());
            }
            // LSE lane (M60 assembler truth, atom.s + one-byte rows:
            // class bits[29:24]==0b111000, Rs!=0 in general — the old
            // `Rm==0` gate belonged to three 0x41-niche words, NOT the
            // lane: stadd/ldadd/swp/cas carry Rs=1/2/4/5): o2==001 is
            // STADD/LDADD/SWP (op=o0: stadd o0==0/L==0, ldadd o0==0,
            // swp o0==1) + B/H/W/X sizes incl. ldaddb; o2==011/111 are
            // the L-variants (staddl/ldaddal/swpl) + CAS (o2==101/111
            // is CAS ONLY when Rs!=0 — plain-CAS 0xC8A07C41 has Rs=0
            // and lives on the exclusive lane above, while LSE-CAS has
            // Rs=comparand!=0 and Rs==31 marks LDAPR).
            // Single-core: plain RMW; Rt=old always (except no-return
            // STADD: Rt==31).
            if o1 == 1 && (o2 == 0b001 || o2 == 0b011) {
                let op = bits(w, 15, 14);
                // LSE size is bits[31:30] DIRECTLY (00=B,01=H,10=W,11=X
                // — atom.s: ldaddb=38.., staddh=78.., stadd= B8..,
                // stadd-x=F8..). NOT sf-derived (sf==1 for BOTH the W
                // stadd B821027F and the X stadd F821027F).
                let size_b: u64 = match bits(w, 31, 30) {
                    0b00 => 1,
                    0b01 => 2,
                    0b10 => 4,
                    _ => 8,
                };
                let addr = self.rsp(rn);
                let old = bus.read(addr, size_b).map_err(|_| Fault::UnmappedData(addr))?;
                let a = self.r(rs) & mask(size_b);
                let b = self.r(rd) & mask(size_b);
                // op: 00=ADD, 01=CLR, 10=EOR, 11=SET; o0==1 selects
                // SWP (plain exchange, ignores the ALU op).
                let res = if o0 == 1 {
                    b
                } else {
                    match op {
                        0b00 => old.wrapping_add(a) & mask(size_b),
                        0b01 => (old & !a) & mask(size_b),
                        0b10 => (old ^ a) & mask(size_b),
                        _ => (old | a) & mask(size_b),
                    }
                };
                bus.write(addr, size_b, res).map_err(|_| Fault::UnmappedData(addr))?;
                if rd != 31 {
                    self.w(rd, old, size_b == 8);
                }
                return Ok(());
            }
            // LSE CAS + LDAPR (o2==101/111, o1==1): Rs==31 marks LDAPR
            // (plain load, o0==1, op==100: b8bfc261/f8bfc261 W/X +
            // B/H forms); else CAS (Rs=comparand, Rt=new, Rt=old).
            if o1 == 1 && (o2 == 0b101 || o2 == 0b111) {
                let size_b: u64 = match bits(w, 31, 30) {
                    0b00 => 1,
                    0b01 => 2,
                    0b10 => 4,
                    _ => 8,
                };
                let addr = self.rsp(rn);
                if rs == 31 {
                    // LDAPR (load-acquire RCpc): plain load.
                    let v = bus.read(addr, size_b).map_err(|_| Fault::UnmappedData(addr))?;
                    self.w(rd, v, size_b == 8);
                    return Ok(());
                }
                let old = bus.read(addr, size_b).map_err(|_| Fault::UnmappedData(addr))?;
                let cmp = self.r(rs) & mask(size_b);
                let new = self.r(rd) & mask(size_b);
                if old == (cmp & mask(size_b)) {
                    bus.write(addr, size_b, new & mask(size_b))
                        .map_err(|_| Fault::UnmappedData(addr))?;
                }
                self.w(rd, old, size_b == 8);
                return Ok(());
            }
            if o0 == 1 && l == 1 && o1 == 0 {
                // LSE atomics (CAS/CASP/SWP/LDADD/...): single-core
                // execute-as-plain-RMW. Decode size/regs by form:
                // CAS: Rs holds comparand, Rt new value.
                // M60 WIDTH (same fix): W=4B, X=8B by bits[31:30].
                let is_pair = bits(w, 30, 30) == 0 && o2 == 1;
                if is_pair {
                    // CASP: compare-and-swap pair (8B or 16B by bit30).
                    let rs2 = bits(w, 10, 10);
                    let _ = (rs2, rn, rd);
                    return Err(ill); // pair CAS: next slice if hit
                }
                let nbytes = 1u64 << bits(w, 31, 30);
                let size_b: u64 = nbytes;
                let addr = self.rsp(rn);
                let old = bus.read(addr, size_b).map_err(|_| Fault::UnmappedData(addr))?;
                let cmp = self.r(rs);
                let new = self.r(rd);
                // CAS semantics: if mem==cmp, mem=new; Rd=old always.
                // SWP/LDADD share the encoding lane; treat unknown
                // op2 as CAS-shape (kernel's early use is CAS loops).
                let op = bits(w, 23, 21);
                if op == 0b000 || op == 0b001 {
                    if old == (cmp & mask(size_b)) {
                        bus.write(addr, size_b, new & mask(size_b))
                            .map_err(|_| Fault::UnmappedData(addr))?;
                    }
                    self.w(rd, old, size_b == 8);
                } else {
                    // SWP/LDADD/LDCLR/LDEOR/LDSET shape: mem=new(op)old,
                    // Rd=old. Implement SWP + ADD exactly; others as SWP
                    // (single-core: ordering-only difference).
                    let res = if op == 0b011 {
                        old.wrapping_add(cmp & mask(size_b)) & mask(size_b)
                    } else {
                        new & mask(size_b)
                    };
                    bus.write(addr, size_b, res).map_err(|_| Fault::UnmappedData(addr))?;
                    self.w(rd, old, size_b == 8);
                }
                return Ok(());
            }
            // One-byte LSE (M57 Linux-track, assembler truth enc-1b):
            // STADD/STCLR/STEOR/STSET (no return register): o0==0,
            // L==0, op==0b000, size==0b11-with-Rt==0b11111. Single-core:
            // plain RMW, no register written back. Encodings: stadd=
            // 0xF820007F stclr=0xF820107F steor=0xF820207F stset=
            // 0xF820307F (+W forms size==0b10, +L acquire forms).
            // The fault word 0xC8047C62 is... NOT this (top8 0xC8 =
            // exclusive lane, not LSE) — it falls through to Err(ill)
            // below, correctly: it is an exclusive-form word the next
            // slice names.
            // M60 WIDTH (same fix): size by bits[31:30].
            if o0 == 0 && l == 0 && bits(w, 23, 21) == 0b000 && rd == 31 {
                let size_b: u64 = 1u64 << bits(w, 31, 30);
                let addr = self.rsp(rn);
                let old = bus.read(addr, size_b).map_err(|_| Fault::UnmappedData(addr))?;
                let v = self.r(rs);
                let op = bits(w, 14, 12);
                let res = match op {
                    0b000 => old.wrapping_add(v & mask(size_b)) & mask(size_b),
                    0b001 => (old & !(v & mask(size_b))) & mask(size_b),
                    0b010 => (old ^ (v & mask(size_b))) & mask(size_b),
                    _ => (old | (v & mask(size_b))) & mask(size_b),
                };
                bus.write(addr, size_b, res).map_err(|_| Fault::UnmappedData(addr))?;
                return Ok(());
            }
            return Err(ill);
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
                // M57 ADRP FIX (spec-correct, kernel-proven): the offset
                // is a SIGNED 21-bit imm (immhi:immlo) scaled by 4K —
                // sign-extend BEFORE the <<12, in i64 space. The old
                // code sext()ed to u64 then wrapping_shl(12), which
                // re-interprets bit63 of the extended value as a NEW
                // sign (0xF...F44F -> 0x2780...000): every negative
                // ADRP landed ~0x2780_0000_0000_0000 too high. The
                // 4M-guest goldens never caught it (their adrp offsets
                // are all positive). ADR (unscaled) was always correct.
                let raw = (((bits(w, 23, 5) << 2) | bits(w, 30, 29)) as u64) as i64;
                let simm = ((raw << 43) >> 43) as i64; // sign-extend 21
                if op == 1 {
                    let base = (pc & !0xfff) as i64;
                    self.w(rd, base.wrapping_add(simm << 12) as u64, true);
                } else {
                    let imm = simm as u64;
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
                    // M57 MOVK-ZERO FIX (kernel-proven): `movk x11,#0`
                    // =0xF280000B has opc=0b11 with imm16=0,pos=0 — the
                    // old KEEP arm `(Rd & !mask)|v` is CORRECT here, but
                    // an earlier variant special-cased it wrong. Keep
                    // the canonical form; the kernel's
                    // movk x11,#0x800,lsl#16 (0xF2A1000B) + movk x11,#0
                    // (0xF280000B) sequence must yield 0x8000000.
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
                // NOTE: Rn/Rm order matters (fixed 2026-09-12): the
                // result is (Rn << (width-lsb)) | (Rm >> lsb) — an
                // earlier version had a/b swapped, which passed the
                // lsb==0 and Rn==Rm (ROR-alias) cases but silently
                // corrupted everything else (proven by multf3's
                // mantissa alignment: 10x10 -> 64.0 instead of 100.0).
                let r = if lsb == 0 {
                    a
                } else {
                    ((a << (width - lsb)) | (b >> lsb)) & m
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

        // AdvSIMD extras (the few vector forms guests use; everything
        // else vector faults like the stock core, so parity holds):
        // DUP-element (.4h from W), MOVI (2d #0, 16b #0x20), EOR
        // (.8b/.16b full-128), lane-extract to FP scalar (H/S). Rows
        // machine-derived (mask,value); Q file holds vectors (top
        // halves zeroed on write — unobservable while other vector
        // ops fault).
        {
            let rd = bits(w, 4, 0);
            let rn = bits(w, 9, 5);
            let rm = bits(w, 20, 16);
            // DUP .4h (exact row; other dup forms fault).
            if (w & 0xffff_fc00) == 0x0e02_0c00 {
                let v = (self.r(rn) & 0xffff) as u128;
                let mut q: u128 = 0;
                for i in 0..4 {
                    q |= v << (16 * i);
                }
                self.q[rd as usize] = q;
                return Ok(());
            }
            // MOVI 2d #0 / 16b #0x20 (exact rows; other immediates fault).
            if (w & 0xffff_ffe0) == 0x6f00_e400 {
                self.q[rd as usize] = 0;
                return Ok(());
            }
            if (w & 0xffff_ffe0) == 0x4f01_e400 {
                self.q[rd as usize] = 0x2020_2020_2020_2020_2020_2020_2020_2020;
                return Ok(());
            }
            // EOR .8b/.16b (full-128 bitwise; arrangement bit selects the
            // row but the operation is identical). Q=0 (64-bit) forms
            // clear the top half (oracle-fitted: mov-8b zeroes it).
            if (w & 0xffe0_fc00) == 0x2e20_1c00 || (w & 0xffe0_fc00) == 0x6e20_1c00 {
                let r = self.q[rn as usize] ^ self.q[rm as usize];
                self.q[rd as usize] = if bits(w, 30, 30) == 0 {
                    r & 0xffff_ffff_ffff_ffff
                } else {
                    r
                };
                return Ok(());
            }
            // ORR .8b/.16b (full-128 bitwise; `mov v.16b` is the ORR
            // alias with Rm == Rn). Rows machine-derived like EOR.
            // Q=0 forms clear the top half (oracle-fitted).
            if (w & 0xffe0_fc00) == 0x0ea0_1c00 || (w & 0xffe0_fc00) == 0x4ea0_1c00 {
                let r = self.q[rn as usize] | self.q[rm as usize];
                self.q[rd as usize] = if bits(w, 30, 30) == 0 {
                    r & 0xffff_ffff_ffff_ffff
                } else {
                    r
                };
                return Ok(());
            }
            // BIT/BIF .8b (firmware's bit-twiddling; Q=0 rows only, as
            // observed — no 16b forms in the image). Operand order is
            // oracle-fitted (see test/simd-oracle.mjs): Vm is the
            // SELECTOR for both (BIT takes Vn where set, BIF takes Vn
            // where clear). Q=0 clears the top half.
            if (w & 0xffe0_fc00) == 0x2ea0_1c00 {
                let qd = self.q[rd as usize];
                let qn = self.q[rn as usize];
                let qm = self.q[rm as usize];
                let r = (qm & qn) | (qd & !qm);
                self.q[rd as usize] = r & 0xffff_ffff_ffff_ffff;
                return Ok(());
            }
            if (w & 0xffe0_fc00) == 0x2ee0_1c00 {
                let qd = self.q[rd as usize];
                let qn = self.q[rn as usize];
                let qm = self.q[rm as usize];
                let r = (qm & qd) | (qn & !qm);
                self.q[rd as usize] = r & 0xffff_ffff_ffff_ffff;
                return Ok(());
            }
            // MOVI D,#0 (scalar; the assembler rejects other D
            // immediates, so the row is exact): zeroes the lane. The
            // top half is zeroed too (fw convention, oracle-fitted).
            if (w & 0xffff_ffe0) == 0x2f00_e400 {
                self.q[rd as usize] = 0;
                return Ok(());
            }
            // FMOV Xd, Vn.D[1] (element extract to general; the index
            // is always 1 — d[0] assembles to the scalar fmov form).
            if (w & 0xffff_fc00) == 0x9eae_0000 {
                if rd != 31 {
                    self.x[rd as usize] = (self.q[rn as usize] >> 64) as u64;
                }
                return Ok(());
            }
            // FMOV Vd.D[1], Xn (element insert from general; index
            // always 1; low lane preserved).
            if (w & 0xffff_fc00) == 0x9eaf_0000 {
                let lo = self.q[rd as usize] & 0xffff_ffff_ffff_ffff;
                self.q[rd as usize] = lo | ((self.r(rn) as u128) << 64);
                return Ok(());
            }
            // FNEG .2d (Q=1 row only, as observed): flip both lane
            // sign bits.
            if (w & 0xffff_fc00) == 0x6ee0_f800 {
                let q = self.q[rn as usize];
                let lo = ((q & 0xffff_ffff_ffff_ffff) as u64 ^ 0x8000_0000_0000_0000) as u128;
                let hi =
                    (((q >> 64) as u64 ^ 0x8000_0000_0000_0000) as u128) << 64;
                self.q[rd as usize] = hi | lo;
                return Ok(());
            }
            // SHL (immediate) D-lane (firmware's (x<<32) idiom):
            // shift = imm7-64, imm7 = bits[22:16] (bit22 fixed 1 by
            // the mask, so 32-bit-element forms fault honestly).
            // Top half zeroed (oracle-fitted, like the Q=0 lanes).
            if (w & 0xffc0_fc00) == 0x5f40_5400 {
                let sh = ((w >> 16) & 0x7f) - 64;
                self.q[rd as usize] = (self.fr(rn).wrapping_shl(sh)) as u128;
                return Ok(());
            }
            // SSHR (immediate) D-lane: arithmetic shift = 128-imm7
            // (1..=64); shift 64 sign-fills (checked_shr would drop
            // the bit, so saturate by hand).
            if (w & 0xffc0_fc00) == 0x5f40_0400 {
                let sh = 128 - ((w >> 16) & 0x7f);
                let v = self.fr(rn);
                let r = if sh >= 64 {
                    if (v >> 63) == 1 { u64::MAX } else { 0 }
                } else {
                    ((v as i64) >> sh) as u64
                };
                self.q[rd as usize] = r as u128;
                return Ok(());
            }
            // BSL .8b/.16b (assembler truth dec51.s; two arrangement rows
            // like EOR; Q=0 clears the top half — oracle-confirmed).
            // Operand order is the ARCHITECTURE order (Vm is the selector:
            // Rd = (Rn&Rm)|(Rd&~Rm), identical to BIT). WARNING: the stock
            // unicorn oracle implements BSL WRONG — truth-table-proven over
            // 7 input classes to compute Rd=(Rn&Rd)|(Rm&~Rd) (Rd as
            // selector, Rm/Rd swapped in the helper), i.e. it behaves as
            // AND when Rd=0. Its BIT/BIF/EOR/ORR are all correct (BIT even
            // agrees with our BSL, same formula), so this is a BSL-only
            // oracle bug — do NOT "fit" BSL to the oracle output. Kept
            // spec-correct; no live guest executes BSL (zero hits in the
            // firmware image).
            if (w & 0xffe0_fc00) == 0x2e60_1c00 || (w & 0xffe0_fc00) == 0x6e60_1c00 {
                let qd = self.q[rd as usize];
                let qn = self.q[rn as usize];
                let qm = self.q[rm as usize];
                let r = (qn & qm) | (qd & !qm);
                self.q[rd as usize] = if bits(w, 30, 30) == 0 {
                    r & 0xffff_ffff_ffff_ffff
                } else {
                    r
                };
                return Ok(());
            }
            // DUP .2d from X (M51): replicate the general register into
            // both double lanes (full Q write, no top question).
            if (w & 0xffff_fc00) == 0x4e08_0c00 {
                let v = self.r(rn) as u128;
                self.q[rd as usize] = v | (v << 64);
                return Ok(());
            }
            // USHR (immediate) D-lane + .2d vector (M51, dec51.s):
            // logical shift = 128-imm7 (imm7 = bits[22:16]); shift 64
            // yields zero. D top half zeroed (oracle-fitted).
            if (w & 0xffc0_fc00) == 0x7f40_0400 {
                let sh = 128 - ((w >> 16) & 0x7f);
                let v = if sh >= 64 { 0 } else { self.fr(rn) >> sh };
                self.q[rd as usize] = v as u128;
                return Ok(());
            }
            if (w & 0xffc0_fc00) == 0x6f40_0400 {
                let sh = 128 - ((w >> 16) & 0x7f);
                let q = self.q[rn as usize];
                let lane = |x: u64| if sh >= 64 { 0 } else { x >> sh };
                self.q[rd as usize] =
                    (lane(q as u64) as u128) | ((lane((q >> 64) as u64) as u128) << 64);
                return Ok(());
            }
            // MOV (element) D-lane (only D moves in the image): dst
            // lane = bit20, src lane = bit14 (both machine-derived
            // from the two observed words); the other lane is preserved.
            if (w & 0xffef_bc00) == 0x6e08_0400 {
                let dlane = (w >> 20) & 1;
                let slane = (w >> 14) & 1;
                let qd = self.q[rd as usize];
                let qn = self.q[rn as usize];
                let lane = if slane == 1 { (qn >> 64) as u64 } else { qn as u64 } as u128;
                self.q[rd as usize] = if dlane == 1 {
                    (qd & 0xffff_ffff_ffff_ffff) | (lane << 64)
                } else {
                    (qd & !0xffff_ffff_ffff_ffffu128) | lane
                };
                return Ok(());
            }
            // Fixed-point scalar converts with fbits=0 (mask
            // 0xFFFFFC00): the firmware's int64<->double traffic stays
            // entirely in the D file — oracle-proven (fcv-oracles):
            // scvtf-fixed Dd=(double)(int64)Dn_bits,
            // fcvtzs-fixed Dd=sat(int64)(double)Dn (toward-zero).
            // They live here (not in the FP table below) because that
            // gate only admits 0x1E/0x9E/0x1F tops. No collision with
            // any general-file row (0x5E top).
            if (w & 0xffff_fc00) == 0x5e61_d800 {
                let v = self.fp_from_int(self.fr(rn), 64, false, true);
                self.fw(rd, v, true);
                return Ok(());
            }
            if (w & 0xffff_fc00) == 0x5ee1_b800 {
                let v = self.fp_to_int64(self.fr(rn), 64, false, 0);
                self.fw(rd, v, true);
                return Ok(());
            }
            // Fixed-point scalar converts with fbits>=1 (the assembler
            // rejects #0 for the scaling forms, so #0 above is a
            // genuinely separate encoding). Masks/values machine-derived
            // (fcv3.s); scale rules fitted from #1/#31/#63 samples and
            // oracle-verified per row (simd-oracle.mjs fcv group):
            // - FP-source to-FP (0x5F/0x7F): fbits = 64-scale6
            //   (scale6 = bits[21:16]); int width is 64.
            // - FP-source to-int (0x1E/0x9E + 0x58/0x59): fbits =
            //   64-scale6 (scale6 = bits[15:10]); dest width from sf.
            // Fixed-to-float DIVIDES by 2^fbits (fixed value = int /
            // 2^fbits); float-to-fixed MULTIPLIES (fixed int = trunc(a
            // x 2^fbits)) — oracle-proven (the -1.5 x2 = -3 case caught
            // an inverted first version). Single-rounding proof (adversarial
            // oracle suite verify-flags.mjs, values + FPSR): the int->float
            // scale is exponent-only (exact, no OF/UF in these ranges) and
            // preserves the significand, so fp_from_int's significance-based
            // IX is already the honest flag for the SCALED quotient; the
            // float->fixed widen (f32->f64) and power-of-2 scale are both
            // exact, leaving fp_to_int64's single truncation (+IOC/IX) as
            // the only rounding. Values + flags match the oracle on all
            // adversarial cases (2^53+1-class ints, boundary products,
            // NaN/inf/subnormal sources, fbits extremes).
            if (w & 0xffc0_fc00) == 0x5f40_e400 {
                let fbits = 64 - bits(w, 21, 16);
                let v = self.fp_from_int(self.fr(rn), 64, false, true);
                let r = f64::from_bits(v) / 2f64.powi(fbits as i32);
                self.fw(rd, r.to_bits(), true);
                return Ok(());
            }
            if (w & 0xffc0_fc00) == 0x7f40_e400 {
                let fbits = 64 - bits(w, 21, 16);
                let v = self.fp_from_int(self.fr(rn), 64, true, true);
                let r = f64::from_bits(v) / 2f64.powi(fbits as i32);
                self.fw(rd, r.to_bits(), true);
                return Ok(());
            }
            // S-float fixed-point mirrors (assembler truth dec51.s, oracle
            // differential verify51.mjs): same scale rules; the integer
            // side is 32-bit (low S bits — nonzero high D/Q bits are
            // ignored, oracle-proven). #0 plains are exact rows
            // (fbits=0, scale ignored — same finding as D). NOTE the
            // UCVTF-plain row (0x7E21D800): the first cut only had the
            // SCVTF one and faulted `ucvtf s0, s0` (oracle executes it).
            if (w & 0xffc0_fc00) == 0x5f00_e400 {
                let fbits = 64 - bits(w, 21, 16);
                let v = self.fp_from_int(self.fr(rn), 32, false, false);
                let r = f32::from_bits(v as u32) / 2f32.powi(fbits as i32);
                self.fw(rd, r.to_bits() as u64, false);
                return Ok(());
            }
            if (w & 0xffc0_fc00) == 0x7f00_e400 {
                let fbits = 64 - bits(w, 21, 16);
                let v = self.fp_from_int(self.fr(rn), 32, true, false);
                let r = f32::from_bits(v as u32) / 2f32.powi(fbits as i32);
                self.fw(rd, r.to_bits() as u64, false);
                return Ok(());
            }
            if (w & 0xffff_fc00) == 0x5e21_d800 {
                let v = self.fp_from_int(self.fr(rn), 32, false, false);
                self.fw(rd, v, false);
                return Ok(());
            }
            if (w & 0xffff_fc00) == 0x7e21_d800 {
                let v = self.fp_from_int(self.fr(rn), 32, true, false);
                self.fw(rd, v, false);
                return Ok(());
            }
            {
                let fcvf = (w & 0xffff_0000) as u32;
                if fcvf == 0x1e58_0000
                    || fcvf == 0x9e58_0000
                    || fcvf == 0x1e59_0000
                    || fcvf == 0x9e59_0000
                {
                    let fbits = 64 - bits(w, 15, 10);
                    let a = f64::from_bits(self.fr(rn)) * 2f64.powi(fbits as i32);
                    let unsigned = fcvf == 0x1e59_0000 || fcvf == 0x9e59_0000;
                    let ibits = if sf { 64 } else { 32 };
                    let v = self.fp_to_int64(a.to_bits(), ibits, unsigned, 0);
                    // Dest width from sf (X=64/W=32); w() zero-extends W.
                    self.w(rd, v, sf);
                    return Ok(());
                }
                // S-float source mirrors (M51, dec51.s): same 64-scale
                // rule; the S value widens exactly to f64 first (so the
                // high D bits never leak in — oracle-proven).
                if fcvf == 0x1e18_0000
                    || fcvf == 0x9e18_0000
                    || fcvf == 0x1e19_0000
                    || fcvf == 0x9e19_0000
                {
                    let fbits = 64 - bits(w, 15, 10);
                    let a = f32::from_bits(self.fr(rn) as u32) as f64 * 2f64.powi(fbits as i32);
                    let unsigned = fcvf == 0x1e19_0000 || fcvf == 0x9e19_0000;
                    let ibits = if sf { 64 } else { 32 };
                    let v = self.fp_to_int64(a.to_bits(), ibits, unsigned, 0);
                    self.w(rd, v, sf);
                    return Ok(());
                }
            }
            // NOTE: lane-extract to FP scalar (`mov hN, vM.h[i]`, top
            // 0x5E) deliberately faults: the stock core faults it too
            // (verified: oracle faults, so executing it here would break
            // fault-both parity; the guests' memset paths never take it).
        }

        // Floating-point data-processing (scalar S/D) + int<->FP moves:
        // flat (mask,value) dispatch — every row machine-derived from
        // aarch64-none-elf-as output (multi-sample const/vary analysis;
        // reg/imm fields masked out, so all register numbers match).
        // Width: ptype = bit22 (0=S,1=D) for the FP side, bit31 (0=W,
        // 1=X) for the integer side. Q/NEON/H/single-prec-vector forms
        // fault (the stock core faults them too, so parity holds).
        // FPSR cumulative flags maintained (FPCR hardwired default RN).
        if ((w >> 24) & 0xff) == 0x1e || ((w >> 24) & 0xff) == 0x9e || ((w >> 24) & 0xff) == 0x1f
        {
            let rd = bits(w, 4, 0);
            let rn = bits(w, 9, 5);
            let rm = bits(w, 20, 16);
            let ra = bits(w, 14, 10);
            let is64 = bits(w, 22, 22) == 1;
            let is64i = bits(w, 31, 31) == 1;
            // 3-reg ALU (fadd/fsub/fmul/fdiv). Ra (bits14:10) is part
            // of the opcode and must be 0 as assembled (kept by the
            // masked values). The ROW selects the op (bit19:18 is 0 for
            // all four — verified, not hand-derived).
            let alu = (w & 0xffe0_fc00) as u32;
            let op = match alu {
                0x1e60_2800 | 0x1e20_2800 => 0, // fadd
                0x1e60_3800 | 0x1e20_3800 => 1, // fsub
                0x1e60_0800 | 0x1e20_0800 => 2, // fmul
                0x1e60_1800 | 0x1e20_1800 => 3, // fdiv
                _ => 99,
            };
            if op != 99 {
                if is64 {
                    let a = f64::from_bits(self.fr(rn));
                    let b = f64::from_bits(self.fr(rm));
                    let (r, invalid, dz, exact) = match op {
                        0b00 => {
                            let r = a + b;
                            let inv = a.is_infinite() && b.is_infinite() && a != b;
                            (r, inv, false, fp_exact_addsub(a, b, r, false))
                        }
                        0b01 => {
                            let r = a - b;
                            let inv = a.is_infinite() && b.is_infinite() && a == b;
                            (r, inv, false, fp_exact_addsub(a, b, r, true))
                        }
                        0b10 => {
                            let r = a * b;
                            let inv = (a == 0.0 && b.is_infinite())
                                || (a.is_infinite() && b == 0.0);
                            (r, inv, false, fp_exact_mul(a, b, r))
                        }
                        _ => {
                            let r = a / b;
                            let inv = (a == 0.0 && b == 0.0)
                                || (a.is_infinite() && b.is_infinite());
                            let dz = b == 0.0 && a.is_finite() && a != 0.0;
                            let ex = r.is_finite() && b != 0.0 && r * b == a;
                            (r, inv, dz, ex)
                        }
                    };
                    let ab = self.fr(rn);
                    let bb = self.fr(rm);
                    let v = self.fp_end_bin64(ab, bb, r, invalid, dz, exact);
                    self.fw(rd, v, true);
                } else {
                    let a = f32::from_bits(self.fr(rn) as u32);
                    let b = f32::from_bits(self.fr(rm) as u32);
                    let (r, invalid, dz, exact) = match op {
                        0b00 => {
                            let r = a + b;
                            let inv = a.is_infinite() && b.is_infinite() && a != b;
                            let ex = fp_exact_addsub(a as f64, b as f64, r as f64, false)
                                && (r as f64) == (a as f64) + (b as f64);
                            (r, inv, false, ex)
                        }
                        0b01 => {
                            let r = a - b;
                            let inv = a.is_infinite() && b.is_infinite() && a == b;
                            let ex = fp_exact_addsub(a as f64, b as f64, r as f64, true)
                                && (r as f64) == (a as f64) - (b as f64);
                            (r, inv, false, ex)
                        }
                        0b10 => {
                            let r = a * b;
                            let inv = (a == 0.0 && b.is_infinite())
                                || (a.is_infinite() && b == 0.0);
                            let ex = fp_exact_mul(a as f64, b as f64, r as f64)
                                && (r as f64) == (a as f64) * (b as f64);
                            (r, inv, false, ex)
                        }
                        _ => {
                            let r = a / b;
                            let inv = (a == 0.0 && b == 0.0)
                                || (a.is_infinite() && b.is_infinite());
                            let dz = b == 0.0 && a.is_finite() && a != 0.0;
                            let ex = r.is_finite() && b != 0.0 && r * b == a;
                            (r, inv, dz, ex)
                        }
                    };
                    let ab = self.fr(rn) as u32;
                    let bb = self.fr(rm) as u32;
                    let v = self.fp_end_bin32(ab, bb, r, invalid, dz, exact);
                    self.fw(rd, v as u64, false);
                }
                return Ok(());
            }
            // 1-source FP (fmov-reg/fneg/fabs/fsqrt/frint*/fcvt): rows
            // keyed by the full (mask 0xFFFFFC00) value.
            let u1 = (w & 0xffff_fc00) as u32;
            // (kind, mode): kind 0=mov 1=neg 2=abs 3=sqrt 4=rint 5=cvt.
            let one: Option<(u32, u32)> = match u1 {
                0x1e60_4000 | 0x1e20_4000 => Some((0, 0)), // fmov-reg
                0x1e61_4000 | 0x1e21_4000 => Some((1, 0)), // fneg
                0x1e20_c000 | 0x1e60_c000 => Some((2, 0)), // fabs
                0x1e61_c000 | 0x1e21_c000 => Some((3, 0)), // fsqrt
                0x1e65_4000 | 0x1e25_4000 => Some((4, 3)), // frintm
                0x1e66_4000 | 0x1e26_4000 => Some((4, 4)), // frinta
                0x1e64_4000 | 0x1e24_4000 => Some((4, 1)), // frintn
                0x1e64_c000 | 0x1e24_c000 => Some((4, 2)), // frintp
                0x1e67_c000 | 0x1e27_c000 => Some((4, 5)), // frinti
                0x1e65_c000 | 0x1e25_c000 => Some((4, 0)), // frintz
                0x1e67_4000 | 0x1e27_4000 => Some((4, 6)), // frintx (mode 6: like i + IXC)
                0x1e62_4000 => Some((5, 1)),               // fcvt S,D (s<-d)
                0x1e22_c000 => Some((5, 0)),               // fcvt D,S (d<-s)
                _ => None,
            };
            if let Some((kind, mode)) = one {
                if is64 {
                    let a = self.fr(rn);
                    match kind {
                        0 => self.fw(rd, a, true),
                        1 => self.fw(rd, (-f64::from_bits(a)).to_bits(), true),
                        2 => self.fw(rd, f64::from_bits(a).abs().to_bits(), true),
                        3 => {
                            let x = f64::from_bits(a);
                            if x.is_nan() {
                                if fp_is_snan64(a) {
                                    self.fpsr_set(0);
                                    self.fw(rd, fp_quiet64(a), true);
                                } else {
                                    self.fw(rd, a, true);
                                }
                            } else if x < 0.0 {
                                self.fpsr_set(0);
                                self.fw(rd, 0x7ff8_0000_0000_0000, true);
                            } else {
                                let r = x.sqrt();
                                if r != 0.0 && r.abs() < f64::MIN_POSITIVE {
                                    self.fpsr_set(3);
                                    self.fpsr_set(4);
                                } else if r * r != x {
                                    self.fpsr_set(4);
                                }
                                self.fw(rd, r.to_bits(), true);
                            }
                        }
                        4 => {
                            let v = self.fp_rint64(a, mode, mode == 6);
                            self.fw(rd, v, true);
                        }
                        _ => {
                            // fcvt S,D only valid with ptype=1 (D source).
                            if !is64 {
                                return Err(ill);
                            }
                            let v = self.fp_narrow(a);
                            self.fw(rd, v as u64, false);
                        }
                    }
                } else {
                    let a = self.fr(rn) as u32;
                    match kind {
                        0 => self.fw(rd, a as u64, false),
                        1 => self.fw(rd, (-f32::from_bits(a)).to_bits() as u64, false),
                        2 => self.fw(rd, f32::from_bits(a).abs().to_bits() as u64, false),
                        3 => {
                            let x = f32::from_bits(a);
                            if x.is_nan() {
                                if fp_is_snan32(a) {
                                    self.fpsr_set(0);
                                    self.fw(rd, fp_quiet32(a) as u64, false);
                                } else {
                                    self.fw(rd, a as u64, false);
                                }
                            } else if x < 0.0 {
                                self.fpsr_set(0);
                                self.fw(rd, 0x7fc0_0000, false);
                            } else {
                                let r = x.sqrt();
                                if r != 0.0 && r.abs() < f32::MIN_POSITIVE {
                                    self.fpsr_set(3);
                                    self.fpsr_set(4);
                                } else if r * r != x {
                                    self.fpsr_set(4);
                                }
                                self.fw(rd, r.to_bits() as u64, false);
                            }
                        }
                        4 => {
                            let v = self.fp_rint32(a, mode, mode == 6);
                            self.fw(rd, v as u64, false);
                        }
                        _ => {
                            // fcvt D,S only valid with ptype=0 (S source).
                            if is64 {
                                return Err(ill);
                            }
                            let v = self.fp_widen(a);
                            self.fw(rd, v, true);
                        }
                    }
                }
                return Ok(());
            }
            // FMOV immediate (mask keeps imm8 out — data, not opcode).
            let mi = (w & 0xffe0_1fe0) as u32;
            if mi == 0x1e60_1000 || mi == 0x1e20_1000 {
                let v = fmov_imm(bits(w, 20, 13), is64);
                self.fw(rd, v, is64);
                return Ok(());
            }
            // Compares: fcmp/fcmpe (reg + #0), fccmp, fcsel.
            let c = (w & 0xffe0_fc1f) as u32;
            // (which, signaling, zero): which 0=cmp 1=cmpe.
            let cmp: Option<(u32, bool, bool)> = match c {
                0x1e60_2000 | 0x1e20_2000 => Some((0, false, false)),
                0x1e60_2008 | 0x1e20_2008 => Some((0, false, true)),
                0x1e60_2010 | 0x1e20_2010 => Some((0, true, false)),
                0x1e60_2018 | 0x1e20_2018 => Some((0, true, true)),
                _ => None,
            };
            if let Some((_, signaling, zero)) = cmp {
                if is64 {
                    if zero {
                        self.fp_cmp64(self.fr(rn), 0, signaling);
                    } else {
                        self.fp_cmp64(self.fr(rn), self.fr(rm), signaling);
                    }
                } else if zero {
                    self.fp_cmp32(self.fr(rn) as u32, 0, signaling);
                } else {
                    self.fp_cmp32(self.fr(rn) as u32, self.fr(rm) as u32, signaling);
                }
                return Ok(());
            }
            let cc = (w & 0xffe0_0c10) as u32;
            if cc == 0x1e20_0400 || cc == 0x1e60_0400 {
                // FCCMP quiet (E=0 kept by the mask; signaling faults).
                // Width = ptype bit22 baked into the row.
                let cond = bits(w, 15, 12);
                let nzcv = bits(w, 3, 0);
                if self.cond_holds(cond) {
                    if cc == 0x1e60_0400 {
                        self.fp_cmp64(self.fr(rn), self.fr(rm), false);
                    } else {
                        self.fp_cmp32(self.fr(rn) as u32, self.fr(rm) as u32, false);
                    }
                } else {
                    self.set_flags(
                        nzcv & 8 != 0,
                        nzcv & 4 != 0,
                        nzcv & 2 != 0,
                        nzcv & 1 != 0,
                    );
                }
                return Ok(());
            }
            let cs = (w & 0xffe0_0c00) as u32;
            if cs == 0x1e60_0c00 || cs == 0x1e20_0c00 {
                let cond = bits(w, 15, 12);
                let take_n = self.cond_holds(cond);
                let v = if take_n { self.fr(rn) } else { self.fr(rm) };
                self.fw(rd, v, cs == 0x1e60_0c00);
                return Ok(());
            }
            // int<->FP moves and converts (mask 0xFFFFFC00).
            let cv = (w & 0xffff_fc00) as u32;
            // (dir, signed, fpmode): dir 0=int->fp 1=fp->int 2=both-bits.
            // fpmode for fp->int: 0=zs 1=zu 2=as.
            let cvop: Option<(u32, bool, u32)> = match cv {
                0x9e67_0000 | 0x1e27_0000 => Some((2, true, 0)), // fmov fp<-int
                0x9e66_0000 | 0x1e26_0000 => Some((2, true, 1)), // fmov int<-fp
                0x9e62_0000 | 0x1e62_0000 | 0x1e22_0000 | 0x9e22_0000 => {
                    Some((0, true, 0)) // scvtf
                }
                0x9e63_0000 | 0x1e63_0000 | 0x1e23_0000 | 0x9e23_0000 => {
                    Some((0, false, 0)) // ucvtf
                }
                0x1e78_0000 | 0x9e78_0000 | 0x1e38_0000 | 0x9e38_0000 => {
                    Some((1, true, 0)) // fcvtzs
                }
                0x1e79_0000 | 0x9e79_0000 | 0x1e39_0000 | 0x9e39_0000 => {
                    Some((1, false, 0)) // fcvtzu
                }
                0x1e64_0000 | 0x9e64_0000 | 0x1e24_0000 | 0x9e24_0000 => {
                    Some((1, true, 4)) // fcvtas
                }
                _ => None,
            };
            if let Some((dir, signed, fpmode)) = cvop {
                // Int-side width from bit31 (X=64/W=32), FP-side from ptype.
                let ibits = if is64i { 64 } else { 32 };
                match dir {
                    0 => {
                        let v = self.fp_from_int(self.r(rn), ibits, !signed, is64);
                        self.fw(rd, v, is64);
                    }
                    1 => {
                        let v = if is64 {
                            self.fp_to_int64(self.fr(rn), ibits, !signed, fpmode)
                        } else {
                            self.fp_to_int32(self.fr(rn) as u32, ibits, !signed, fpmode)
                        };
                        self.w(rd, v, ibits == 64);
                    }
                    _ => {
                        if fpmode == 0 {
                            // int -> fp bits.
                            if is64 {
                                self.fw(rd, self.r(rn), true);
                            } else {
                                self.fw(rd, self.r(rn), false);
                            }
                        } else if is64 {
                            self.w(rd, self.fr(rn), true);
                        } else {
                            self.w(rd, self.fr(rn), false);
                        }
                    }
                }
                return Ok(());
            }
            // Fused multiply-add family (mask keeps o1/o0 + ptype).
            let ma = (w & 0xffe0_8000) as u32;
            // Row: 0=fmadd 1=fmsub 2=fnmadd 3=fnmsub.
            // FORK QUIRK (probed with a=2,b=3,c=4: fork gives
            // 10,-2,-10,+2): the fork swaps FMSUB<->FNMSUB negation —
            // its FMSUB computes -(a*b)+c and its FNMSUB computes
            // (a*b)-c. Mirrored here for parity (FMADD/FNMADD match
            // spec and are kept).
            let fma: Option<u32> = match ma {
                0x1f40_0000 | 0x1f00_0000 => Some(0), // fmadd: ab+c
                0x1f40_8000 | 0x1f00_8000 => Some(1), // fmsub-row: -(ab)+c (fork)
                0x1f60_0000 | 0x1f20_0000 => Some(2), // fnmadd: -(ab+c)
                0x1f60_8000 | 0x1f20_8000 => Some(3), // fnmsub-row: (ab)-c (fork)
                _ => None,
            };
            if let Some(which) = fma {
                if is64 {
                    let a = f64::from_bits(self.fr(rn));
                    let b = f64::from_bits(self.fr(rm));
                    let cc = f64::from_bits(self.fr(ra));
                    let ab = self.fr(rn);
                    let bb = self.fr(rm);
                    let cb = self.fr(ra);
                    let minv = (a == 0.0 && b.is_infinite())
                        || (a.is_infinite() && b == 0.0);
                    // First SNaN in (a,b,c) order, quieted preserving
                    // sign+payload (fork-verified).
                    let sn = if fp_is_snan64(ab) {
                        Some(ab)
                    } else if fp_is_snan64(bb) {
                        Some(bb)
                    } else if fp_is_snan64(cb) {
                        Some(cb)
                    } else {
                        None
                    };
                    if minv || sn.is_some() {
                        self.fpsr_set(0);
                        self.fw(
                            rd,
                            sn.map(fp_quiet64).unwrap_or(0x7ff8_0000_0000_0000),
                            true,
                        );
                        return Ok(());
                    }
                    // Per-row fused form (negation is exact, so these
                    // round identically to the parenthesized spec forms).
                    let r = match which {
                        0 => a.mul_add(b, cc),        // ab+c
                        1 => (-a).mul_add(b, cc),     // -(ab)+c (fork)
                        2 => -(a.mul_add(b, cc)),     // -(ab+c)
                        _ => a.mul_add(b, -cc),       // (ab)-c (fork)
                    };
                    if a.is_nan() || b.is_nan() || cc.is_nan() {
                        self.fw(rd, r.to_bits(), true);
                        return Ok(());
                    }
                    let exact = a == 0.0 || b == 0.0;
                    let v = self.fp_end_bin64(ab, bb, r, false, false, exact);
                    self.fw(rd, v, true);
                } else {
                    let a = f32::from_bits(self.fr(rn) as u32);
                    let b = f32::from_bits(self.fr(rm) as u32);
                    let cc = f32::from_bits(self.fr(ra) as u32);
                    let ab = self.fr(rn) as u32;
                    let bb = self.fr(rm) as u32;
                    let cb = self.fr(ra) as u32;
                    let minv = (a == 0.0 && b.is_infinite())
                        || (a.is_infinite() && b == 0.0);
                    let sn = if fp_is_snan32(ab) {
                        Some(ab)
                    } else if fp_is_snan32(bb) {
                        Some(bb)
                    } else if fp_is_snan32(cb) {
                        Some(cb)
                    } else {
                        None
                    };
                    if minv || sn.is_some() {
                        self.fpsr_set(0);
                        self.fw(
                            rd,
                            sn.map(fp_quiet32).unwrap_or(0x7fc0_0000) as u64,
                            false,
                        );
                        return Ok(());
                    }
                    let r = match which {
                        0 => a.mul_add(b, cc),
                        1 => (-a).mul_add(b, cc),
                        2 => -(a.mul_add(b, cc)),
                        _ => a.mul_add(b, -cc),
                    };
                    if a.is_nan() || b.is_nan() || cc.is_nan() {
                        self.fw(rd, r.to_bits() as u64, false);
                        return Ok(());
                    }
                    let exact = a == 0.0 || b == 0.0;
                    let v = self.fp_end_bin32(ab, bb, r, false, false, exact);
                    self.fw(rd, v as u64, false);
                }
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
            let extended = bits(w, 21, 21) == 1;
            let b = if !extended {
                shift_reg(self.r(rm), bits(w, 23, 22), bits(w, 15, 10), sf)
            } else {
                extend_reg(
                    self.r(rm),
                    bits(w, 15, 13),
                    bits(w, 12, 10) as u32,
                )
            };
            // Rn==31: SP everywhere EXCEPT shifted-form SUB (S=0 or S=1),
            // where it reads XZR. Proven by `neg x1, x1` (SUB-shifted,
            // must compute 0-x1; reading SP gave SP-x1 = 0x3FFAAF and a
            // wild store fault in firmware): GNU encodes NEG with Rn=31
            // intending XZR. (The old rule keyed on S and got it
            // backwards.) Extended forms keep SP (fuzz-pinned by
            // `add x6, sp, w7, uxtx` etc.), as does shifted ADD.
            let a = if rn == 31 && !extended && op == 1 {
                self.r(rn)
            } else {
                self.rsp(rn)
            };
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
                // reads XZR. Second operand is imm5 (bits20:16) when
                // bit11 is set, else register Rm (assembler ground truth:
                // ccmp-imm sets bit11, ccmp-reg clears it — the old code
                // always read X[imm], so `ccmp w0, #5` compared against
                // X5 and broke `<` (fell into the == handler) and `!=`.
                let a = self.r(rn);
                let b = if bits(w, 11, 11) == 1 {
                    rm as u64
                } else {
                    self.r(rm)
                };
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

// ---- M56 Linux-track loader (raw blobs at fixed PAs) ----

/// ARM64 Linux boot layout on pi-cpu (matches the qemu raspi3ap oracle:
/// `-m 512M`, kernel raw `Image`, DTB + initramfs as separate blobs).
/// All fit in `LINUX_RAM_SIZE` (512M); none fit in legacy 4M RAM.
pub const LINUX_KERNEL_PA: u64 = 0x200000;
pub const LINUX_DTB_PA: u64 = 0x3000000;
pub const LINUX_INITRD_PA: u64 = 0x4000000;

/// Write a raw blob at a physical address (Linux-mode RAM).
/// Bounds-checked against `ram_size()`; errors name the blob.
fn load_raw(bus: &mut Bus, pa: u64, bytes: &[u8], name: &str) -> Result<(), String> {
    let end = pa
        .checked_add(bytes.len() as u64)
        .ok_or_else(|| format!("{} wraps", name))?;
    if end > bus.ram_size() {
        return Err(format!(
            "{} 0x{:x}..0x{:x} outside RAM 0x{:x}",
            name,
            pa,
            end,
            bus.ram_size()
        ));
    }
    bus.mem[pa as usize..end as usize].copy_from_slice(bytes);
    Ok(())
}

/// Load a real Pi 3 boot (raw kernel `Image` + DTB + initrd cpio.gz) at
/// the fixed PAs above. Expands RAM to 512M (`linux_mode`), zeroes the
/// extra RAM (fresh `vec!`, so stale 4M contents never leak), and
/// returns the kernel entry PA (= `LINUX_KERNEL_PA`; the ARM64 `Image`
/// header has no ELF entry — execution starts at the load address).
/// Boot regs (x0=DTB PA, x1=x2=x3=0, MMU off, EL2) are the caller's job
/// (see `Cpu::linux_reset`); DTB `chosen` patching (bootargs +
/// `linux,initrd-start/end`) happens HERE, not in the harness — every
/// caller (triage, wasm demo, future shells) gets a bootable DTB.
/// Patched in place after the DTB blob lands: `bootargs` =
/// earlycon+console+maxcpus+mem (qemu-oracle cmdline, initrd variant:
/// no root= — /init owns the mount), plus `linux,initrd-start` =
/// LINUX_INITRD_PA and `linux,initrd-end` = PA+len (u64 cells, like
/// the qemu `-initrd` setup). No-op if /chosen is missing.
pub fn load_linux(
    bus: &mut Bus,
    kernel: &[u8],
    dtb: &[u8],
    initrd: &[u8],
) -> Result<u64, String> {
    bus.mem = vec![0; LINUX_RAM_SIZE as usize];
    bus.linux_mode = true;
    load_raw(bus, LINUX_KERNEL_PA, kernel, "kernel")?;
    load_raw(bus, LINUX_DTB_PA, dtb, "dtb")?;
    load_raw(bus, LINUX_INITRD_PA, initrd, "initrd")?;
    patch_dtb_chosen(bus, initrd.len() as u64);
    // SD backing for the Linux rootfs (M60 rootfs slice): the .data
    // initrd region is the qemu-oracle ext2 SD image (magic-proven at
    // +1080: 0x53EF), booted by the oracle via -drive if=sd with
    // root=/dev/mmcblk0. Back the PIO disk with the SAME bytes so the
    // kernel's SDHCI driver reads the real partition table/superblock
    // instead of the 5-sector FAT12 toy (whose MBR magic the driver
    // rejects — proven: mmc0 scout found no card, no SD census at 1B).
    // Cap: sd_import caps at 32 sectors (16K); lift here by direct
    // sector copy (the image is 8192 sectors / 4 MiB, fits RAM easily).
    {
        let nsec = initrd.len() / 512;
        bus.sd_disk.clear();
        bus.sd_disk.reserve(nsec);
        for chunk in initrd.chunks_exact(512) {
            let mut sec = [0u8; 512];
            sec.copy_from_slice(chunk);
            bus.sd_disk.push(sec);
        }
    }
    Ok(LINUX_KERNEL_PA)
}

/// M59 DTB patch: bootargs + initrd addresses into /chosen, in place.
/// FDT layout (big-endian): header 40B (magic/totalsize/off_struct/
fn be32(b: &[u8], o: usize) -> u32 {
    ((b[o] as u32) << 24) | ((b[o + 1] as u32) << 16) | ((b[o + 2] as u32) << 8) | (b[o + 3] as u32)
}

/// Append a u32 cell (big-endian) to a Vec<u8> DTB scratch buffer.
fn put32(v: &mut Vec<u8>, x: u32) {
    v.push((x >> 24) as u8);
    v.push((x >> 16) as u8);
    v.push((x >> 8) as u8);
    v.push(x as u8);
}

/// M59 DTB patch, reimplemented: rebuild the whole FDT with three extra
/// /chosen props (bootargs + linux,initrd-start/end), preserving every
/// existing node/prop byte-for-byte.
fn patch_dtb_chosen(bus: &mut Bus, initrd_len: u64) {
    const HDR: usize = 40;
    let base = LINUX_DTB_PA as usize;
    if bus.mem.len() < base + HDR {
        return;
    }
    if be32(&bus.mem, base) != 0xd00dfeed {
        return;
    }
    let totalsize = be32(&bus.mem, base + 4) as usize;
    let off_struct = be32(&bus.mem, base + 8) as usize;
    let off_strings = be32(&bus.mem, base + 12) as usize;
    let size_strings = be32(&bus.mem, base + 32) as usize;
    let size_struct = be32(&bus.mem, base + 36) as usize;
    if base + totalsize > bus.mem.len() {
        return;
    }
    // Sanity: struct block must end where the strings block begins.
    if off_struct + size_struct > off_strings || off_strings + size_strings > totalsize {
        return;
    }
    // Find the END_NODE token closing /chosen (stack walk). FDTv17
    // layout (this DTB: version 17, NO FDT_NOP tokens — the first-cut
    // walker expected NOPs at 0x88 and died with token 24, so chosen_end
    // stayed None and the patch silently no-op'd): tokens are only
    // BEGIN_NODE=1 / END_NODE=2 / PROP=3 / END=9. Token 9 (FDT_END)
    // terminates the block — handle it, don't break: the struct walker
    // must consume it (memcheck2-proven: token 9 right after memory@0's
    // props; break skips the node-END + chosen node and chosen_end
    // stays None... in practice chosen precedes memory here so it
    // worked, but correctness first). Node-name alignment: pad to 4
    // RELATIVE to the struct-block start (spec: "aligned to a 32-bit
    // boundary"), NOT to the DTB base — the first cut used (o - sb)
    // which is the same thing (o starts at sb), kept.
    let sb = base + off_struct;
    let se = sb + size_struct;
    let mut o = sb;
    let mut stack: Vec<Vec<u8>> = Vec::new();
    let mut chosen_end: Option<usize> = None;
    while o + 4 <= se {
        let t = be32(&bus.mem, o);
        if t == 1 {
            let mut i = o + 4;
            while bus.mem[i] != 0 {
                i += 1;
            }
            stack.push(bus.mem[o + 4..i].to_vec());
            o = i + 1;
            o += (4 - ((o - sb) % 4)) % 4;
        } else if t == 2 {
            if stack.last().map(|s| s.as_slice()) == Some(b"chosen".as_slice()) {
                chosen_end = Some(o);
            }
            stack.pop();
            o += 4;
        } else if t == 3 {
            let len = be32(&bus.mem, o + 4) as usize;
            o += 12 + len + ((4 - (len % 4)) % 4);
        } else if t == 4 {
            // FDT_NOP (FDTv17, this DTB uses them as padding): skip.
            // The first-cut walker broke on any other token, which made
            // chosen_end stay None on NOP-padded DTBs and the whole
            // patch silently no-op (bootargs/initrd/memory never
            // applied — DTB-proven by the raw-struct parse).
            o += 4;
        } else if t == 9 {
            o += 4;
            break;
        } else {
            break;
        }
    }
    let chosen_end = match chosen_end {
        Some(v) => v,
        None => return,
    };
    // New props to append (name + value bytes).
    // M59 bootargs DEDUP (proven by dbgdtb: the stock DTB already HAS a
    // bootargs prop, so naive append leaves TWO bootargs and the kernel
    // reads the first = stock cmdline with no console=ttyAMA0, no
    // maxcpus, no mem= — silent spin with no UART). Rebuild the struct
    // block WITHOUT the old bootargs prop, then append ours. The old
    // prop's string bytes stay (harmless orphans).
    // M60 bootargs (oracle-matched: public/linux/module.js KERNEL_COMMON
    // + minimal blacklist; the M59 earlycon/maxcpus/mem= line starved
    // the CMA allocator and panicked at __unflatten_device_tree).
    // M61: the blacklist is byte-exact public/linux/module.js `minimal`
    // (dwc2/xhci/usb-storage/sdhci-iproc/i2c/spi/rng/sound/... skipped):
    // the single-entry M60 line let the kernel probe stub hardware whose
    // models return zeros/timeout (firmware-clock WARN at 3.4s, then the
    // mmc/usb/sdhci probes stall the boot at vgaarb with sd census 0).
    // Skipping them is what the WORKING oracle boots with — the drivers
    // re-enable one model at a time as their pi-cpu models land.
    // Root (oracle module.js MT variant): the .data initrd region is an
    // ext2 SD image (magic-proven), booted via -drive if=sd, so the
    // cmdline MUST carry root=/dev/mmcblk0 rootwait (verified: without
    // it the 6.1.21 kernel idr-finds its root disk forever at
    // b91e38 — 1B insns fault-null with zero UART).
    // earlycon (ktock oracle examples/raspi3ap module.js): the prebuilt
    // kernel needs earlycon=pl011,0x3f201000 for pre-console-init
    // output; M24 "no earlycon" applies to the qemu-wasm TCG slow path
    // only. pi-cpu UART writes are a RAM tap (free), so earlycon can
    // only help visibility, never slow execution.
    // M68 maxcpus=1 (single-core honesty): pi-cpu runs ONE core (the
    // SmpRunner exists only for the bare-metal smp guest; the Linux
    // runner never starts cores 1..3), but the DTB advertises 4
    // spin-table CPUs — without maxcpus the kernel spends ~3000s of
    // virtual time waiting on each secondary's spin-table release
    // ("CPU1/2/3: failed to come online ... failed in unknown state"
    // then "Brought up 1 node, 1 CPU" — qemu boots all 4, pi-cpu
    // cannot yet). maxcpus=1 tells the kernel to skip secondaries and
    // matches the emulation reality. The "EFI services will not be
    // available" line just above it is NORMAL on every platform
    // without UEFI (qemu raspi3ap prints it too) — not an error.
    let bootargs = b"earlycon=pl011,0x3f201000 console=ttyAMA0,115200 lpj=7000000 nokaslr mitigations=off nowatchdog nosoftlockup audit=0 cgroup_disable=memory ipv6.disable=1 cryptomgr.notests loglevel=8 maxcpus=1 root=/dev/mmcblk0 rootfstype=ext4 rootwait initcall_blacklist=bcm2835_pm_driver_init,bcm2835_cpufreq_init,bcm2835_wdt_init,leds-gpio,thermal,gpio-fan,pwm-fan,dwc2,xhci-hcd,smsc95xx,usb_ernet,rndis_host,cdc_ether,usb-storage,sdhci-iproc,i2c-bcm2835,spi-bcm2835,bcm2835-rng,brcmstb_thermal,snd_bcm2835,vchiq,snd_pcm,snd_timer,snd,soundcore,joydev,rfkill,bcm2835_v4l2,cfg80211,rfkill_gpio\0";
    // M60 memory@0 FIX (console-proven: the stock DTB's memory@0 reg is
    // all-ZERO — qemu fills real RAM size via -m 512M; without it the
    // kernel sees 0 bytes, CMA fails, panic at
    // early_init_dt_alloc_memory_arch): patch reg to <0 0x20000000>
    // (address-cells=1, size-cells=1: base 0, 512M). Same rebuild path
    // as bootargs (skip old reg, append fixed). Node name INCLUDES the
    // unit address ("memory@0", python-proven — not "memory").
    let start = LINUX_INITRD_PA;
    let end = start + initrd_len;
    let mut sbytes: Vec<u8> = Vec::new();
    sbytes.extend_from_slice(b"bootargs\0");
    sbytes.extend_from_slice(b"linux,initrd-start\0");
    sbytes.extend_from_slice(b"linux,initrd-end\0");
    let mut props: Vec<u8> = Vec::new();
    let mut str_off = size_strings;
    let mut emit = |name_off: usize, val: &[u8], out: &mut Vec<u8>| {
        put32(out, 3);
        put32(out, val.len() as u32);
        put32(out, name_off as u32);
        out.extend_from_slice(val);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    };
    emit(str_off, bootargs, &mut props);
    str_off += 9; // "bootargs\0"
    let mut u64be = |v: u64, out: &mut Vec<u8>| {
        put32(out, (v >> 32) as u32);
        put32(out, v as u32);
    };
    let mut vstart = Vec::new();
    u64be(start, &mut vstart);
    emit(str_off, &vstart, &mut props);
    str_off += 19; // "linux,initrd-start\0"
    let mut vend = Vec::new();
    u64be(end, &mut vend);
    emit(str_off, &vend, &mut props);
    // Rebuild: head40 + struct[..chosen_end] + props +
    // struct[chosen_end..struct_end] + strings + new names, with the
    // header offsets fixed up. (off_struct is unchanged: the reserve
    // map sits at [40..off_struct) and we never touch it.)
    // The old bootargs prop (if any) is SKIPPED: copy struct[..chosen]
    // in two runs — before the old prop and after it — so the rebuilt
    // /chosen has exactly one bootargs (ours). Locate it by scanning
    // the chosen range for a PROP whose name is "bootargs".
    // M60 memory@0 reg uses the SAME skip-and-append path (same code,
    // different node/prop): locate the memory@0 node's reg prop.
    let find_prop = |node: &[u8], prop: &[u8], end: usize| -> Option<(usize, usize)> {
        let mut found = None;
        let mut p = sb;
        let mut stack: Vec<Vec<u8>> = Vec::new();
        while p + 4 <= end {
            let t = be32(&bus.mem, p);
            if t == 1 {
                let mut i = p + 4;
                while bus.mem[i] != 0 {
                    i += 1;
                }
                stack.push(bus.mem[p + 4..i].to_vec());
                p = i + 1;
                p += (4 - ((p - sb) % 4)) % 4;
            } else if t == 2 {
                stack.pop();
                p += 4;
            } else if t == 3 {
                let len = be32(&bus.mem, p + 4) as usize;
                let nm = be32(&bus.mem, p + 8) as usize;
                let ns = base + off_strings + nm;
                let mut j = ns;
                while bus.mem[j] != 0 {
                    j += 1;
                }
                // Node match: last stack element starts with node bytes
                // (memory@0 — unit address included, exact here).
                let node_ok = stack
                    .last()
                    .map(|s| s.as_slice() == node)
                    .unwrap_or(false);
                if node_ok && &bus.mem[ns..j] == prop {
                    found = Some((p, 12 + len + ((4 - (len % 4)) % 4)));
                    break;
                }
                p += 12 + len + ((4 - (len % 4)) % 4);
            } else if t == 4 {
                // FDT_NOP: skip (see the chosen_end walker above).
                p += 4;
            } else if t == 9 {
                break;
            } else {
                break;
            }
        }
        found
    };
    let bootargs_name_off: Option<(usize, usize)> = find_prop(b"chosen", b"bootargs", chosen_end);
    // memory reg: scan the WHOLE struct block (node is outside /chosen).
    // NOTE the node name INCLUDES the unit address (python-proven:
    // BEGIN "memory@0", not "memory" — the earlier walk that printed
    // "memory" stripped it).
    let memreg_off: Option<(usize, usize)> =
        find_prop(b"memory@0", b"reg", sb + size_struct);
    // New memory@0 reg value: <0 0x20000000> (address-cells=1,
    // size-cells=1: base 0, 512M = LINUX_RAM_SIZE). NOTE the node name
    // is "memory" here (NO unit address — python-walk-proven: BEGIN
    // "memory@0" never appears; the node is BEGIN "memory"), so match
    // b"memory".
    let mut memreg = Vec::new();
    put32(&mut memreg, 0);
    put32(&mut memreg, LINUX_RAM_SIZE as u32);
    let mut memprop = Vec::new();
    // NO new string bytes: "reg" already exists in the strings block
    // (dbgfind3: the name resolves empty because off_strings is STALE at
    // emit time — the ORIGINAL bug. Reuse the OLD reg prop's name offset
    // instead, read from the old prop header before skipping it.)
    let memreg_name_off: usize = memreg_off
        .map(|(pp, _)| be32(&bus.mem, pp + 8) as usize)
        .unwrap_or(0);
    emit(memreg_name_off, &memreg, &mut memprop);
    let mut out: Vec<u8> = Vec::new();
    // Copy with BOTH old props skipped. Layout (python-proven): /chosen
    // ENDs at chosen_end (o_struct 1288); memory@0's reg is AFTER it
    // (o_struct 23472). So: HEAD = base..sb (FDT header + reserve map,
    // COPIED VERBATIM — never skip here) + A = sb..chosen_end minus
    // bootargs, then props + memprop at chosen_end, then
    // C = chosen_end..struct_end minus memreg, gap, strings, new names.
    // (M60 CORRUPTION FIX, dbgfind2-proven: the old code copied
    // base..pp with pp-ET-al as ABSOLUTE bus.mem offsets — but the
    // finders return STRUCT-RELATIVE... no: they return absolute. The
    // REAL bug: `out` starts at base, so `out[4]` is totalsize — but
    // after extend, out[40..] is reserve map ONLY if we copied base..sb
    // first. The old code copied base..pp where pp >= sb, so that held.
    // Actual corruption: hdr() patches out[4/12/32/36] — correct. So
    // why token 147? Because memreg_off was None (memory@0 never
    // matched: find_prop compares stack-top == b"memory@0" — dbgfind
    // PUSHED "memory@0" and PROP matched node_ok=true... yet memcheck2
    // shows the OLD zero reg. So the skip never applied AND the new
    // reg landed at chosen_end (inside /chosen!) — kernel reads
    // chosen/reg as garbage + memory stays zero. Two bugs: (1) memprop
    // must NOT go at chosen_end (it belongs in memory@0); (2) the old
    // reg must actually be skipped. Fix both: insert memprop AT the
    // memory@0 node end, not at chosen_end.)
    let mut dropped = 0usize;
    let struct_end = sb + size_struct;
    // A: base..chosen_end minus bootargs skip (chosen props only).
    if let Some((pp, pl)) = bootargs_name_off {
        out.extend_from_slice(&bus.mem[base..pp]);
        out.extend_from_slice(&bus.mem[pp + pl..chosen_end]);
        dropped += pl;
    } else {
        out.extend_from_slice(&bus.mem[base..chosen_end]);
    }
    // Chosen additions go at chosen_end (inside /chosen — correct).
    out.extend_from_slice(&props);
    // C: chosen_end..struct_end minus the OLD memory reg skip. The NEW
    // memory reg is spliced AT the memory@0 node: i.e. right where the
    // old reg prop was (pp..pp+pl replaced by memprop). Everywhere else
    // copied verbatim.
    if let Some((pp, pl)) = memreg_off {
        out.extend_from_slice(&bus.mem[chosen_end..pp]);
        out.extend_from_slice(&memprop);
        out.extend_from_slice(&bus.mem[pp + pl..struct_end]);
        dropped += pl;
    } else {
        out.extend_from_slice(&bus.mem[chosen_end..struct_end]);
    }
    // Reserve-map gap (struct_end..strings) + old strings + new names.
    out.extend_from_slice(&bus.mem[struct_end..base + off_strings]);
    out.extend_from_slice(&bus.mem[base + off_strings..base + off_strings + size_strings]);
    out.extend_from_slice(&sbytes);
    let new_size_struct = size_struct + props.len() + memprop.len() - dropped;
    // strings block: only the THREE chosen names are new ("reg" reused).
    let new_size_strings = size_strings + sbytes.len();
    let new_off_strings = off_strings + props.len() + memprop.len() - dropped;
    let new_totalsize = new_off_strings + new_size_strings;
    // patch header in `out`
    let mut hdr = |o: usize, v: u32| {
        out[o] = (v >> 24) as u8;
        out[o + 1] = (v >> 16) as u8;
        out[o + 2] = (v >> 8) as u8;
        out[o + 3] = v as u8;
    };
    hdr(4, new_totalsize as u32);
    hdr(12, new_off_strings as u32);
    hdr(32, new_size_strings as u32);
    hdr(36, new_size_struct as u32);
    // Bounds: DTB region must still fit (32K headroom is plenty: +~150B).
    if base + new_totalsize > bus.mem.len() {
        return;
    }
    bus.mem[base..base + new_totalsize].copy_from_slice(&out[..new_totalsize]);
}

