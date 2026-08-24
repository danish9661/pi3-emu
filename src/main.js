import './styles.css';
import { parseElf, loadElf } from './elf.js';
import { mmuEnable, mmuMirrorWrite } from './mmu.js';
import { dmaRunChain } from './dma.js';
import { createPwm } from './pwm.js';
import { createI2c } from './i2c.js';
import { createSpi } from './spi.js';
import { createUart1 } from './uart1.js';
import { createUart0 } from './uart0.js';
import { createSdhci } from './sdhci.js';
import { createLocalInt } from './localint.js';
import { createIc } from './ic.js';
import { createGpio } from './gpio.js';

const UART_WINDOW = 0x1000;
const RAM_BASE = 0x0;
const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const MAX_SLICES = 5000;

// Linux boot (M23): a real arm64 kernel Image loaded at 0x200000 (the entry
// is base + the Image header's text_offset), the stock bcm2837-rpi-3-b DTB
// at 0x3000000 (x0 = dtb, x1..x3 = 0, MMU off — the arm64 boot protocol).
// RAM grows to 128 MB, and every peripheral address we don't model gets a
// zero-return "black hole" map so stray driver probes fail gracefully
// instead of taking a data abort. The arch timer counter is ticked at the
// real 19.2 MHz rate every slice (Linux reads CNTPCT for timekeeping), and
// IRQ delivery is real (CPU_INTERRUPT_HARD via the local block, native
// vectors/eret) like LIRQ_MODE.
const LINUX_MODE = 'linux';
// The arm64 boot protocol wants a 2M-aligned base (text_offset=0). The fork
// cannot fetch at pc 0 (a TB-gen quirk), so the image goes at 0x200000: the
// kernel's tables map VA KIMAGE+X -> phys 0x200000+X, and the fork's
// truncated fetches of KIMAGE VAs (0x08000000+X) resolve through the
// pre-seeded idmap aliases (seedIdmapAliases). The old 0x80000 load made
// the kernel's own mapping run 0x80000 off — everything faulted at slice
// 4243 (see test/linux-probe.mjs).
const LINUX_IMAGE = 0x200000;
const LINUX_DTB = 0x3000000;

// SMP mailbox window (host-arbitrated device shared by all cores):
//   +0x00 START_ENTRY[3]  core 0 writes entry addresses for cores 1..3
//   +0x10 GO              core 0 releases the secondaries
//   +0x14 COUNTER         shared counter (device-serialized increment)
//   +0x18 LOCK            spinlock flag
//   +0x1C MSG[4]          per-core result mailbox
//   +0x30 CURRENT         running core id (host-written)
//   +0x34 PARK_MASK       core i sets bit i when done
//   +0x38 CPUID           host-written core id
// BCM2837 system timer: host refreshes CLO/CHI from wall clock before each
// slice (epoch = program boot, so the counter starts near 0 and ticks in us).
//  +0x00 CS   match flags M0..M3 (host sets as CLO passes each compare)
//  +0x04 CLO  counter low  (host)
//  +0x08 CHI  counter high (host)
//  +0x0C..0x18 C0..C3 compare registers (guest)
//  +0x20 DONE host extension: clock guest parks by writing 1
const TMR_BASE = 0x3F003000;
const TMR_CS = TMR_BASE + 0x00;
const TMR_CLO = TMR_BASE + 0x04;
const TMR_CMP = TMR_BASE + 0x0c;
const TMR_DONE = TMR_BASE + 0x20;
const CLOCK_MODE = 'clock';
const CLOCK_MAX_SLICES = 60000;

// BCM2837 GPIO (see src/gpio.js): the real register layout with event
// registers and two IRQ lines. Output levels are tracked host-side, inputs
// are host-driven (the UI button), edges are detected at slice boundaries.
// The 8 LEDs live on pins 21..28, the button on pin 29.
const GPIO_BASE = 0x3F200000;
const GPIO_LEDS = [21, 22, 23, 24, 25, 26, 27, 28];
const GPIO_BTN = 29;
const GPIO_MODE = 'gpio';

// BCM2837 mailbox (VideoCore interface, real layout). The guest writes a
// tag-buffer address to MAIL1_WRITE (channel 8); the host, playing the
// VideoCore, parses the request at the slice boundary, answers the known
// tags into guest RAM, then posts the address back to MAIL0_READ and
// clears the empty bit in MAIL0_STATUS.
//  +0x00 MAIL0_READ      host: reply address | channel
//  +0x04 MAIL0_STATUS    host: bit31 = empty
//  +0x14 MAIL1_WRITE     guest: request address | channel
//  +0x18 MAIL1_STATUS    host: bit31 = full (never set; writes always taken)
const MBOX_BASE = 0x3F00B880;
const MBOX_WINDOW = 0x3F00B000; // unicorn mem_map needs a 4K-aligned base
const MBOX_READ = MBOX_BASE + 0x00;
const MBOX_STATUS = MBOX_BASE + 0x04;
const MBOX_MAIL1_WRITE = MBOX_BASE + 0x14;
const MBOX_MAIL1_STATUS = MBOX_BASE + 0x18;
const MBOX_CHANNEL = 8;

const SMP_BASE = 0x3F202000;
const SMP_START = SMP_BASE + 0x00;
const SMP_GO = SMP_BASE + 0x10;
const SMP_COUNTER = SMP_BASE + 0x14;
const SMP_LOCK = SMP_BASE + 0x18;
const SMP_MSG = SMP_BASE + 0x1c;
const SMP_CURRENT = SMP_BASE + 0x30;
const SMP_PARK = SMP_BASE + 0x34;
const SMP_CPUID = SMP_BASE + 0x38;
const CORE_COUNT = 4;
const MAX_ROUNDS = 2000;
const SMP_MODE = 'smp';

// Host-assisted MMU (see mmu.js): this unicorn build cannot run a guest with
// SCTLR.M set (msr sctlr_el1 traps, MAIR_EL1 unimplemented), so the host
// provides translation. The guest writes rootPa | 1 to +0x00 to enable
// (0 disables); the host walks the guest's page tables, maps non-identity
// blocks at their VA with a shadow copy, and mirrors writes both ways.
const MMU_CTL = 0x3F00D000;
const MMU_DONE = MMU_CTL + 0x04; // host extension: guest writes 1 when finished
const MMU_MAX_SLICES = 30000;
let mmuState = null;
let mmuHook = null;
let mmuCtl = 0;
let mmuDone = false;

// Host-arbitrated BCM2835 DMA controller (see dma.js): channel 0 registers at
// 0x3F007000 (CS +0x00, CONBLK_AD +0x04), the real ENABLE at 0x3F00E050, and a
// DONE host extension at +0x54. ACTIVE starts the chain; the host performs the
// transfers between slices and latches CS.END + CS.INT (INTEN on the final CB
// drives the IC's DMA0 line, bit 16).
const DMA_BASE = 0x3F007000;
const DMA_CS = DMA_BASE + 0x00;
const DMA_CONBLK = DMA_BASE + 0x04;
const DMA_ENABLE = 0x3F00E050;
const DMA_DONE = 0x3F00E054;
const DMA_MAX_SLICES = 30000;
let dmaInt = false;
let dmaEnd = false;
let dmaEnable = 0;
let dmaLastCS = 0;
let dmaDone = false;
let dmaIntSeen = false;

// Host-arbitrated BCM2835 PWM controller (see pwm.js): FIFO-mode model at
// 0x3F20C000 with a DONE host extension at +0x54. The browser plays the
// drained sample ring through WebAudio.
const PWM_BASE = 0x3F20C000;
const PWM_MAX_SLICES = 30000;
let pwmSyncOut = null;
let pwmSyncIn = null;
let pwmState = null;
let pwmAudioFed = 0;
let audioCtx = null;
let audioRing = null;
let audioPos = 0;
let audioLen = 0;

// Host-arbitrated I2C (BSC) master (see i2c.js): a sensor slave at 0x3F804000
// with a DONE host extension at +0x54. The guest parks by writing I2C_DONE.
const I2C_BASE = 0x3F804000;
const I2C_MAX_SLICES = 30000;
let i2cSyncOut = null;
let i2cSyncIn = null;
let i2cState = null;

// Host-arbitrated SPI0 master (see spi.js): a flash-slave JEDEC responder at
// 0x3F204000 with a DONE host extension at +0x54. Guest parks on SPI_DONE.
const SPI_BASE = 0x3F204000;
const SPI_MAX_SLICES = 30000;
let spiSyncOut = null;
let spiSyncIn = null;
let spiState = null;

// Host-arbitrated AUX mini UART (UART1) at 0x3F215000 (see uart1.js): a
// second console, output-only. Its chars are tagged "[u1] " per line. The
// guest parks on getc like the plain programs, so runUntilIdle applies.
const UART1_BASE = 0x3F215000;
let uart1SyncOut = null;
let uart1SyncIn = null;
let uart1LineStart = true; // next UART1 char starts a line (add the [u1] tag)

// PL011 UART0 (see uart0.js): the real BCM2837 register model at
// 0x3F201000. TX is emitted by a DR write hook, RX keys are queued by the
// host (uart0Push) and popped by the guest's DR reads, and RXINTR/TXINTR
// (IMSC bits 4/5) drive the interrupt controller's IRQ 57 line (bank 2,
// bit 25).
let uart0SyncOut = null;
let uart0SyncIn = null;
let uart0State = null;
let uart0Push = null;
let uart0IrqActive = null;

// Host-arbitrated SDHCI (EMMC) controller (see sdhci.js): a FAT12 microSD
// card at 0x3F300000 with a DONE host extension at +0x54. Guest parks on
// SD_DONE after printing HELLO.TXT. IRPT_EN/IRPT_MASK (0x34/0x38) gate the
// IRQ line (bank-2 bit 30, IRQ 62).
const SD_BASE = 0x3F300000;
const SD_MAX_SLICES = 30000;
let sdSyncOut = null;
let sdSyncIn = null;
let sdState = null;
let sdIrqActive = null;

export const PROGRAMS = {
  shell: 'shell.elf',
  sum: 'sum.elf',
  fib: 'fib.elf',
  smp: 'smp.elf',
  clock: 'clock.elf',
  gpio: 'gpio.elf',
  fb: 'fb.elf',
  irq: 'irq.elf',
  mmu: 'mmu.elf',
  dma: 'dma.elf',
  pwm: 'pwm.elf',
  i2c: 'i2c.elf',
  spi: 'spi.elf',
  uart1: 'uart1.elf',
  sd: 'sd.elf',
  uart0: 'uart0.elf',
  lirq: 'lirq.elf',
};

const term = document.getElementById('term');
const status = document.getElementById('status');
const runBtn = document.getElementById('run');
const progSel = document.getElementById('prog');
const statsEl = document.getElementById('stats');
const hint = document.getElementById('hint');
const gpioPanel = document.getElementById('gpiopanel');
const fbCanvas = document.getElementById('fbscreen');
const fbCtx = fbCanvas.getContext('2d');
const gpioLedsEl = document.getElementById('gpio-leds');
const gpioBtnEl = document.getElementById('gpio-btn');

let ucMod = null;
let uc = null;
let board = null;
let mode = 'single';
let cores = null; // smp: one unicorn instance per core
let entries = null; // smp: per-core resume/entry addresses
let smpState = null; // smp: host-arbitrated mailbox state
let tmrWall0 = 0; // timer epoch: performance.now() at program boot
let tmrPending = 0; // CS match bits not yet cleared by the guest
let tmrCrossed = [false, false, false, false]; // compare fired (edge-triggered)
let tmrCompares = [0, 0, 0, 0];
let tmrLastCS = 0; // last CS value the host wrote (detect guest writes)
let clockDone = false;
let mbxLastWrite = 0; // last MAIL1_WRITE value the host mirrored
let mbxPending = false; // a reply is ready in MAIL0_READ
let mbxAddr = 0; // reply address | channel

let gpioBtn = 0; // host-driven input: button pin high while held
let gpio = null; // src/gpio.js model instance
let gpioLedEls = null; // DOM spans for the LED panel
let ic = null; // src/ic.js legacy interrupt controller instance

let stats = { steps: 0, insns: 0, emuMs: 0, chars: 0, wallStart: 0 };

function setStatus(text) {
  status.textContent = text;
}

function draw(text) {
  term.textContent += text;
  term.scrollTop = term.scrollHeight;
}

async function loadBoard() {
  const resp = await fetch('./pi_board.wasm');
  const { instance } = await WebAssembly.instantiate(await resp.arrayBuffer(), {});
  return instance.exports;
}

// The programs are static bare-metal AArch64 ELFs; the host maps RAM + the
// UART window and loads the ELF segments at their vaddrs. The CPU starts at
// e_entry (passed to emu_start — reg_write is a no-op in this unicorn build)
// and the guest's own _start sets SP, so the host never touches registers.
// After that the guest runs freely: each slice is SLICE_INSNS of real
// AArch64 instructions, resuming from the current PC.
function boot(ucMod, uc, board, elf, opts = {}) {
  const uart = Number(board.pi_uart_base());
  const ramSize = opts.ramSize || RAM_SIZE;

  uc.mem_map(RAM_BASE, ramSize, ucMod.PROT_ALL);
  uc.mem_map(uart, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(TMR_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(MBOX_WINDOW, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(GPIO_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(MMU_CTL, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(DMA_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(0x3F00E000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(PWM_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(I2C_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(SPI_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(UART1_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(SD_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(LOCAL_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  gpio = createGpio(uc, ucMod, GPIO_BASE, {
    getBtn: () => gpioBtn << GPIO_BTN,
    onIrqChange: () => rearmGpuLine(uc),
  });
  ic = createIc(uc, ucMod, IC_BASE, icLines);
  localInt = createLocalInt(uc, ucMod, LOCAL_BASE, localLines);
  uc.hook_add(
    ucMod.HOOK_MEM_WRITE,
    (u, access, addr, size, value) => tmrCsHook(u, access, Number(addr), Number(size), value),
    null,
    TMR_CS,
    TMR_CS + 3
  );
  uc.devUart = { base: uart };
  // The PL011 model (src/uart0.js): TX chars are emitted at DR-write time
  // (same hook trick as the mini UART, so UART0 and UART1 chars keep the
  // guest's exact write order), RX keys are queued by the host and popped
  // by the guest's DR reads, and RXINTR/TXINTR drive the IC's IRQ 57 line.
  uart0SyncOut = null;
  uart0SyncIn = null;
  const uart0 = createUart0(
    uc,
    ucMod,
    uart,
    (b) => {
      stats.chars++;
      board.pi_cons_push(b);
    },
    () => rearmGpuLine(uc)
  );
  uart0SyncOut = uart0.syncOut;
  uart0SyncIn = uart0.syncIn;
  uart0State = uart0.state;
  uart0Push = uart0.push;
  uart0IrqActive = uart0.irqActive;
  uc.entry = elf.entry;
  tmrWall0 = performance.now();
  tmrPending = 0;
  tmrCrossed = [false, false, false, false];
  tmrCompares = [0, 0, 0, 0];
  tmrLastCS = 0;
  mbxLastWrite = 0;
  mbxPending = false;
  mbxAddr = 0;
  gpioBtn = 0;
  fbW = 0;
  fbH = 0;
  fbDepth = 0;
  fbPitch = 0;
  fbReady = false;
  irqElr = 0;
  irqInFlight = false;
  irqVector = 0;
  irqResume = 0;
  if (mmuHook) {
    uc.hook_del(mmuHook);
    mmuHook = null;
  }
  mmuState = null;
  mmuCtl = 0;
  dmaInt = false;
  dmaEnd = false;
  dmaEnable = 0;
  dmaLastCS = 0;
  dmaDone = false;
  dmaIntSeen = false;
  const pwm = createPwm(uc, ucMod, PWM_BASE);
  pwmSyncOut = pwm.syncOut;
  pwmSyncIn = pwm.syncIn;
  pwmState = pwm.state;
  pwmAudioFed = 0;
  audioPos = 0;
  audioLen = 0;

  const i2c = createI2c(uc, ucMod, I2C_BASE);
  i2cSyncOut = i2c.syncOut;
  i2cSyncIn = i2c.syncIn;
  i2cState = i2c.state;

  const spi = createSpi(uc, ucMod, SPI_BASE);
  spiSyncOut = spi.syncOut;
  spiSyncIn = spi.syncIn;
  spiState = spi.state;

  const uart1 = createUart1(uc, ucMod, UART1_BASE, uart1Emit);
  uart1SyncOut = uart1.syncOut;
  uart1SyncIn = uart1.syncIn;
  uart1LineStart = true;

  const sd = createSdhci(uc, ucMod, SD_BASE, () => rearmGpuLine(uc));
  sdSyncOut = sd.syncOut;
  sdSyncIn = sd.syncIn;
  sdState = sd.state;
  sdIrqActive = sd.irqActive;

  if (opts.linux) {
    // Map every peripheral address outside the modeled windows as plain
    // zeroed RAM — a driver probing an unmodeled device reads 0 and fails
    // gracefully instead of taking a data abort on unmapped memory.
    mapBlackHole(
      uc,
      ucMod,
      0x3f000000,
      0x3fa00000,
      [
        [TMR_BASE, 0x1000],
        [DMA_BASE, 0x1000],
        [MBOX_WINDOW, 0x1000],
        [MMU_CTL, 0x1000],
        [0x3f00e000, 0x1000],
        [GPIO_BASE, 0x1000],
        [uart, 0x1000],
        [SPI_BASE, 0x1000],
        [PWM_BASE, 0x1000],
        [UART1_BASE, 0x1000],
        [SD_BASE, 0x1000],
        [I2C_BASE, 0x1000],
      ]
    );
    const { image, dtb: dtbRaw } = opts.linux;
    const dtb = new Uint8Array(dtbRaw);
    patchDtbRam(dtb); // memory@0 size -> 128 MB (we only mapped 128 MB)
    const textOffset = Number(
      BigInt(image[8] | (image[9] << 8) | (image[10] << 16) | (image[11] << 24)) |
        ((BigInt(image[12]) | (BigInt(image[13]) << 8n) | (BigInt(image[14]) << 16n) | (BigInt(image[15]) << 24n)) << 32n)
    );
    const entry = LINUX_IMAGE + textOffset;
    writeAll(uc, LINUX_IMAGE, image);
    seedIdmapAliases(uc); // fork fetch aliases for the truncated KIMAGE VAs
    writeAll(uc, LINUX_DTB, patchDtbChosen(dtb)); // earlycon + console bootargs
    uc.entry = entry;
    uc.reg_write(ucMod.ARM64_REG_SP, ramSize - 0x10000);
    uc.reg_write(ucMod.ARM64_REG_X0, LINUX_DTB);
    uc.reg_write(ucMod.ARM64_REG_X1, 0);
    uc.reg_write(ucMod.ARM64_REG_X2, 0);
    uc.reg_write(ucMod.ARM64_REG_X3, 0);
  } else {
    loadElf(uc, elf);
  }
}

// Map every address in [lo, hi) not covered by one of the modeled windows
// (base,size pairs) as plain RAM, in the smallest number of contiguous maps.
// unicorn.js mem_write truncates large payloads (~16 MB); chunk the write.
function writeAll(uc, addr, bytes) {
  const CHUNK = 0x100000;
  for (let off = 0; off < bytes.length; off += CHUNK) {
    uc.mem_write(addr + off, Array.from(bytes.subarray(off, off + CHUNK)));
  }
}
function mapBlackHole(uc, ucMod, lo, hi, skip) {
  const runs = skip.map(([b, s]) => [b, b + s]).sort((a, b) => a[0] - b[0]);
  let cur = lo;
  for (const [b, e] of runs) {
    if (b > cur) uc.mem_map(cur, b - cur, ucMod.PROT_READ | ucMod.PROT_WRITE);
    if (e > cur) cur = e;
  }
  if (hi > cur) uc.mem_map(cur, hi - cur, ucMod.PROT_READ | ucMod.PROT_WRITE);
}

// Patch the DTB's memory@0 reg size (offset of the <0 0x40000000> size cell)
// to 128 MB so the kernel only touches RAM we actually mapped.
function patchDtbRam(dtb) {
  const pat = [0x40, 0x00, 0x00, 0x00];
  for (let i = 0; i + 8 <= dtb.length; i++) {
    if (dtb[i] === 0 && dtb[i + 1] === 0 && dtb[i + 2] === 0 && dtb[i + 3] === 0 &&
        dtb[i + 4] === pat[0] && dtb[i + 5] === pat[1] && dtb[i + 6] === pat[2] && dtb[i + 7] === pat[3]) {
      dtb[i + 4] = 0x08; // 0x08000000
      return true;
    }
  }
  return false;
}

// Insert a "bootargs" property into the /chosen node and rebuild the FDT
// (append the new string to the strings block). Returns the new buffer.
function patchDtbChosen(dtb) {
  const be32 = (o) => (((dtb[o] << 24) | (dtb[o + 1] << 16) | (dtb[o + 2] << 8) | dtb[o + 3]) >>> 0);
  const put32 = (b, o, v) => {
    b[o] = (v >>> 24) & 0xff; b[o + 1] = (v >>> 16) & 0xff; b[o + 2] = (v >>> 8) & 0xff; b[o + 3] = v & 0xff;
  };
  const offStruct = be32(8), offStrings = be32(12), sizeStruct = be32(36), sizeStrings = be32(32);
  const bootargs = 'earlycon=pl011,0x3f201000 console=ttyAMA0,115200';
  const val = new Uint8Array(bootargs.length + 1);
  for (let i = 0; i < bootargs.length; i++) val[i] = bootargs.charCodeAt(i);
  const propPad = (4 - (val.length % 4)) % 4;
  let o = offStruct, chosenEnd = -1;
  const stack = [];
  while (o < offStruct + sizeStruct) {
    const t = be32(o);
    if (t === 1) {
      let i = o + 4; while (dtb[i] !== 0) i++;
      stack.push(String.fromCharCode(...dtb.subarray(o + 4, i)));
      o = i + 1 + ((4 - ((i - o - 4 + 1) % 4)) % 4);
    } else if (t === 2) {
      if (stack.length && stack[stack.length - 1] === 'chosen') chosenEnd = o;
      stack.pop();
      o += 4;
    } else if (t === 3) {
      const len = be32(o + 4);
      o += 12 + len + ((4 - (len % 4)) % 4);
    } else break;
  }
  if (chosenEnd < 0) return dtb;
  const strOff = sizeStrings;
  const prop = new Uint8Array(12 + val.length + propPad);
  put32(prop, 0, 3); // FDT_PROP
  put32(prop, 4, val.length);
  put32(prop, 8, strOff);
  prop.set(val, 12);
  const nbuf = new Uint8Array(dtb.length + prop.length + 9);
  nbuf.set(dtb.subarray(0, chosenEnd));
  nbuf.set(prop, chosenEnd);
  nbuf.set(dtb.subarray(chosenEnd, offStrings), chosenEnd + prop.length);
  nbuf.set(dtb.subarray(offStrings, offStrings + sizeStrings), offStrings + prop.length);
  for (let i = 0; i < 8; i++) nbuf[offStrings + prop.length + sizeStrings + i] = 'bootargs\0'.charCodeAt(i);
  nbuf[offStrings + prop.length + sizeStrings + 8] = 0;
  nbuf[0] = 0xd0; nbuf[1] = 0x0d; nbuf[2] = 0xfe; nbuf[3] = 0xed;
  put32(nbuf, 4, nbuf.length); // totalsize
  put32(nbuf, 8, offStruct); // off_dt_struct (unchanged)
  put32(nbuf, 12, offStrings + prop.length); // off_dt_strings (shifted)
  put32(nbuf, 32, sizeStrings + 9); // size_dt_strings
  put32(nbuf, 36, sizeStruct + prop.length); // size_dt_struct
  return nbuf;
}

// The fork's fetch path truncates VAs to 32 bits and walks them through
// TTBR0 (the idmap). The kernel's own create_idmap only fills the idmap L2
// entries for phys [0, 0x2866000) (blocks 0..0x14), so pre-seed the idmap
// L2 (phys 0x19e2000 = base 0x200000 + init_idmap_pg_dir image offset
// 0x17e0000 + 0x2000, per System.map) with:
//   [0x40..0x51] = 2M blocks mapping truncated KIMAGE VAs back to image phys
//   [0x80]       = a table pointer to an L3 page for the KPTI trampoline
// The L3 page is the reserved_pg_dir page @0x1908000 (the kernel loads it as
// the empty early ttbr1 and never writes it) with the trampoline's 3 pages
// (phys 0x1902000..0x1905000, not 2M-aligned) as 4K entries.
function seedIdmapAliases(uc) {
  const BASE = LINUX_IMAGE;
  const u64le = (v) => [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff, 0, 0, 0, 0];
  const idmapL2 = BASE + 0x17e2000;
  const blocks = [];
  for (let i = 0x40; i <= 0x51; i++) blocks.push(...u64le((BASE + ((i - 0x40) << 21)) | 0x701));
  uc.mem_write(idmapL2 + 0x40 * 8, blocks);
  uc.mem_write(idmapL2 + 0x80 * 8, u64le(BASE + 0x1708003));
  uc.mem_write(BASE + 0x1708000, [...u64le(BASE + 0x1702003), ...u64le(BASE + 0x1703003), ...u64le(BASE + 0x1704003)]);
}

// The "VideoCore": answers a property-tags request buffer the guest wrote
// the address of via MAIL1_WRITE. Known tags get success (0x80000000) with
// board-identity values; unknown tags get an error code. Values are
// serialized as tsize little-endian bytes (host memory is byte-addressed).
function leBytes(n, len) {
  const b = [];
  for (let i = 0; i < len; i++) b.push(Number((BigInt(n) >> BigInt(i * 8)) & 0xffn));
  return b;
}
const MBOX_TAGS = {
  0x00010001: (v, ts) => leBytes(16968947, ts), // GET_FIRMWARE_REVISION
  0x00010002: (v, ts) => leBytes(0xa02082, ts), // GET_BOARD_REVISION (Pi 3 Model B)
  0x00010003: (v, ts) => leBytes(0xdeadbeef00000000n, ts), // GET_BOARD_SERIAL
  0x00010005: (v, ts) => leBytes(0, 4).concat(leBytes(0x400000, 4)), // GET_ARM_MEMORY base,size
  0x00010009: (v, ts) => [0xb8, 0x27, 0xeb, 0xde, 0xad, 0xbe], // GET_MAC_ADDRESS
  0x00030001: (v, ts) => leBytes(v, 4).concat(leBytes(1, 4)), // GET_POWER_STATE device,on
  0x00030002: (v, ts) => leBytes(700000000, ts), // GET_CLOCK_RATE (ARM)
};

function mboxProcess(uc) {
  if (mbxPending) return;
  const addr = mbxAddr & ~0xf;
  const size = Math.min(readU32(uc, addr) & 0xffff, 1024);
  if (size < 8) return;
  let off = 8;
  while (off + 8 <= size) {
    const id = readU32(uc, addr + off);
    if (id === 0) break; // end-of-tags marker
    const tsize = readU32(uc, addr + off + 4);
    if (fbTag(uc, addr, off, id, tsize)) {
      // handled by the framebuffer path
    } else {
      const tag = MBOX_TAGS[id];
      if (tag) {
        const out = tag(readU32(uc, addr + off + 12), tsize);
        writeU32(uc, addr + off + 8, 0x80000000);
        for (let i = 0; i < tsize; i++) {
          uc.mem_write(addr + off + 12 + i, [out[i] ?? 0]);
        }
      } else {
        writeU32(uc, addr + off + 8, 0x80000001); // unknown tag -> error
      }
    }
    off += 12 + tsize + ((4 - (tsize % 4)) % 4);
  }
  writeU32(uc, addr + 4, 0x80000000); // whole request succeeded
  mbxPending = true;
}

// The "VideoCore" display: the guest sets a mode (physical/virtual W/H,
// depth, pixel order) and allocates a buffer; the host carves it out of
// guest RAM and blits it to the canvas every animation frame.
const FB_MODE = 'fb';
let fbFrame = 0;
const IRQ_MODE = 'irq';
let irqFrame = 0;
const LIRQ_MODE = 'lirq';
const FB_ADDR = 0x200000; // allocated framebuffer inside guest RAM
let fbW = 0;
let fbH = 0;
let fbDepth = 0;
let fbPitch = 0;
let fbReady = false;

function fbTag(uc, addr, off, id, tsize) {
  const v = addr + off + 12;
  switch (id) {
    case 0x00048003: // SET_PHYSICAL_W/H
    case 0x00048004: // SET_VIRTUAL_W/H
      fbW = readU32(uc, v);
      fbH = readU32(uc, v + 4);
      writeU32(uc, addr + off + 8, 0x80000000);
      return true;
    case 0x00048005: // SET_DEPTH
      fbDepth = readU32(uc, v);
      writeU32(uc, addr + off + 8, 0x80000000);
      return true;
    case 0x00048006: // SET_PIXEL_ORDER (0 = RGB byte order)
      writeU32(uc, addr + off + 8, 0x80000000);
      return true;
    case 0x00040001: // ALLOCATE_BUFFER: address + pitch
      fbPitch = fbW * 4;
      writeU32(uc, addr + off + 8, 0x80000000);
      writeU32(uc, v, FB_ADDR);
      writeU32(uc, v + 4, fbPitch);
      fbReady = fbW > 0 && fbH > 0 && fbDepth === 32;
      return true;
    case 0x00040008: // GET_PITCH
      writeU32(uc, addr + off + 8, 0x80000000);
      writeU32(uc, v, fbPitch);
      return true;
    default:
      return false;
  }
}

// ---- BCM2835 interrupt controller (0x3F00B200): the real 3-bank layout
// (src/ic.js) — bank 1 (0x04/0x10/0x1C): system timer C0-C3 bits 0-3,
// DMA0 bit 16, AUX bit 29; bank 2 (0x08/0x14/0x20): GPIO bits 17/18,
// PL011 bit 25, SDHCI bit 30; basic (0x00/0x18/0x24) with any-bank bits
// 8/9 + shortcut mirrors 10-20. Pending windows show (line & enabled).
// The GPU line into the local block is the OR of every gated line. ----

// BCM2836 ARM-local interrupt controller: per-core IRQ source registers
// (0x40000060+4n) reported through a window, with real delivery — the host
// drives CPU_INTERRUPT_HARD via uc.arm64_set_irq (src/localint.js).
const LOCAL_BASE = 0x40000000;
const IC_BASE = 0x3F00B200;

let localInt = null;

// Host device lines -> the legacy IC (see src/ic.js).
function icLines() {
  return {
    timer: tmrPending & 0xf,
    dma0: dmaInt && (dmaEnable & 1) !== 0,
    pl011: uart0IrqActive ? uart0IrqActive() : false,
    sdhci: sdIrqActive ? sdIrqActive() : false,
    gpio0: gpio ? gpio.irqActive(0) : false,
    gpio1: gpio ? gpio.irqActive(1) : false,
    aux: false,
  };
}

// The GPU IRQ line into the local interrupt block: any pending-and-enabled
// legacy IC bit raises it.
function gpuLine() {
  return ic ? ic.line() : 0;
}

// The local interrupt block's source lines: arch-timer bits come straight
// from the core (the gt lines the timer path drives), GPU from the legacy
// IC aggregation above.
function localLines() {
  return {
    cntps: uc.arm64_debug(13) ? 1 : 0,
    cntpns: uc.arm64_debug(3) ? 1 : 0,
    cnthp: uc.arm64_debug(12) ? 1 : 0,
    cntv: uc.arm64_debug(11) ? 1 : 0,
    gpu: gpuLine(),
    pmu: 0,
    axi: 0,
    ltimer: 0,
    mailbox: [0, 0, 0, 0],
  };
}

function syncLocalOut(uc) {
  if (!localInt) return;
  localInt.syncOut(uc);
  // Only LIRQ_MODE drives the real CPU_INTERRUPT_HARD line: the legacy-IC
  // modes deliver host-assisted at slice boundaries (irqDeliver records
  // irqElr for the magic-resume glue) and a real mid-slice entry there
  // would resume at PC 0 (no irqElr).
  if (mode === LIRQ_MODE || mode === LINUX_MODE) localInt.syncIrq(uc, (level) => uc.arm64_set_irq(level));
}

function syncLocalIn(uc) {
  if (!localInt) return;
  localInt.syncIn(uc);
}

// Real-time IRQ de-assert: a guest ack (TMR_CS, GPEDS W1C, PL011 RX drain,
// SDHCI W1C) must drop the local block's line before the handler eret's, or
// the stale high level re-triggers delivery (the next slice-boundary sync
// would be too late). Only LIRQ_MODE drives the real CPU_INTERRUPT_HARD
// line: in the legacy-IC modes (IRQ_MODE/uart0/gpio) delivery is host-
// assisted at slice boundaries (irqDeliver records irqElr), and a real
// mid-slice entry would leave that machinery with no resume point.
function rearmGpuLine(uc) {
  if (localInt && (mode === LIRQ_MODE || mode === LINUX_MODE)) localInt.syncIrq(uc, (level) => uc.arm64_set_irq(level));
}

// A TMR_CS ack (or any CS write) from the guest re-derives the GPU line in
// real time.
function tmrCsHook(uc, access, addr, size, value) {
  tmrPending &= Number(value) & 0xf;
  rearmGpuLine(uc);
}

// Deliver a pending, enabled IRQ at a slice boundary. reg_write(PC/ELR) is a
// no-op in this unicorn build, so the delivery is host-assisted: the next
// slice *starts* at the IRQ vector (VBAR + 0x280), and the vector stub
// signals IRQ_RET when the handler is done — the host then resumes the guest
// at the saved PC. While a handler is in flight no further delivery happens.
// Delivery is gated on DAIF.I being clear (uc_arm64_debug(1), bit 7) — the
// same mask the real hardware checks — so a guest with interrupts masked is
// never entered at the vector.
const IC_IRQ_RET = 0x3F00B22C;
let irqElr = 0;
let irqInFlight = false;
let irqVector = 0;
let irqResume = 0;

function daifI() {
  return ((Number(uc.arm64_debug(1)) >> 7) & 1) === 1;
}

function irqDeliver(uc) {
  if (irqInFlight || mode === SMP_MODE || mode === LINUX_MODE) return;
  if (!ic || daifI()) return;
  // ic.pending() is derived fresh from the device lines on every call (the
  // pending windows are refreshed pre-slice, so they are already gated).
  const p = ic.pending();
  if ((p.b1 | p.b2 | p.basic) === 0) return;
  irqElr = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || uc.entry;
  irqInFlight = true;
  const vbar = Number(uc.reg_read_i32(ucMod.ARM64_REG_VBAR_EL1)) || 0x100000;
  irqVector = vbar + 0x280;
}

// Pull the guest's "handler finished" write out of the IC window.
function syncIrqRet(uc) {
  const r = readU32(uc, IC_IRQ_RET);
  if (r !== 0) {
    writeU32(uc, IC_IRQ_RET, 0);
    irqResume = irqElr;
  }
}

// Mirror device state into the guest before a slice; pull guest requests out.
function syncMailboxOut(uc) {
  writeU32(uc, MBOX_MAIL1_STATUS, 0);
  if (mbxPending) {
    writeU32(uc, MBOX_STATUS, 0); // not empty: reply ready
    writeU32(uc, MBOX_READ, mbxAddr);
  } else {
    writeU32(uc, MBOX_STATUS, 0x80000000); // empty
    writeU32(uc, MBOX_READ, 0);
  }
}

function syncMailboxIn(uc) {
  const w = readU32(uc, MBOX_MAIL1_WRITE);
  if (w !== mbxLastWrite) {
    mbxLastWrite = w;
    mbxAddr = w;
    if ((w & 0xf) === MBOX_CHANNEL) mboxProcess(uc);
  }
}

// The 8 LED dots mirror the guest-driven levels (red = on).
function updateGpioPanel() {
  if (mode !== GPIO_MODE || !gpioPanel || !gpio) return;
  if (!gpioLedEls) {
    gpioLedEls = [];
    for (const p of GPIO_LEDS) {
      const el = document.createElement('span');
      el.className = 'led';
      el.title = 'GPIO ' + p;
      gpioLedsEl.appendChild(el);
      gpioLedEls.push(el);
    }
  }
  const out = gpio.state.out;
  for (let i = 0; i < GPIO_LEDS.length; i++) {
    gpioLedEls[i].classList.toggle('on', (out & (1 << GPIO_LEDS[i])) !== 0);
  }
}

// Refresh the timer device from the wall clock before a slice runs: CLO/CHI
// tick in microseconds since program boot, and each compare register sets
// its CS match bit once when the counter first crosses it (edge-triggered).
// The guest clears a bit by rewriting the status mask (host-arbitrated
// memory can only observe byte changes, so CS here is write-mask, not the
// real BCM2837's W1C); a monotonic counter never re-fires a cleared compare.
function syncTimerOut(uc) {
  const us = ((performance.now() - tmrWall0) * 1000) & 0xffffffff;
  writeU32(uc, TMR_CLO, us);
  writeU32(uc, TMR_CLO + 4, 0);
  for (let i = 0; i < 4; i++) {
    const c = tmrCompares[i];
    if (!tmrCrossed[i] && c !== 0 && ((us - c) & 0x80000000) === 0) {
      tmrCrossed[i] = true;
      tmrPending |= 1 << i;
    }
  }
  writeU32(uc, TMR_CS, tmrPending);
  tmrLastCS = tmrPending;
}

function syncTimerIn(uc) {
  for (let i = 0; i < 4; i++) {
    const c = readU32(uc, TMR_CMP + i * 4);
    if (c !== tmrCompares[i]) tmrCrossed[i] = false; // re-armed compare
    tmrCompares[i] = c;
  }
  const cs = readU32(uc, TMR_CS);
  if (cs !== tmrLastCS) tmrPending &= cs & 0xf; // guest rewrote the status mask
  if (mode === CLOCK_MODE || mode === GPIO_MODE) {
    const d = readU32(uc, TMR_DONE);
    if (d !== 0) clockDone = true;
  }
}

function drain(board) {
  let out = '';
  for (;;) {
    const c = Number(board.pi_cons_poll());
    if (c === -1 || c === 0xffffffff) break;
    out += String.fromCharCode(c);
  }
  return out;
}

// ---- SMP: 4 cores, per-core private RAM at the same addresses (partitioned
// DDR), one host-arbitrated MMIO mailbox window as the only shared state. ----

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}

function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}

// Push the host's view of the mailbox into the core's device window before
// it runs (the device the core sees IS the arbiter's commit state).
function syncDeviceOut(c, coreId) {
  writeU32(c, SMP_CPUID, coreId);
  writeU32(c, SMP_CURRENT, coreId);
  writeU32(c, SMP_GO, smpState.go);
  writeU32(c, SMP_COUNTER, smpState.counter);
  writeU32(c, SMP_LOCK, smpState.lock);
  writeU32(c, SMP_PARK, smpState.park);
  for (let i = 0; i < CORE_COUNT; i++) writeU32(c, SMP_MSG + i * 4, smpState.msg[i]);
}

// Pull whatever the core wrote back into host state (commit after each slice).
function syncDeviceIn(c, coreId) {
  if (coreId === 0) {
    for (let i = 0; i < 3; i++) {
      const v = readU32(c, SMP_START + (i + 1) * 4);
      if (v !== 0 && smpState.start[i] === 0) smpState.start[i] = v;
    }
    const g = readU32(c, SMP_GO);
    if (g !== 0) smpState.go = g;
  }
  const ctr = readU32(c, SMP_COUNTER);
  if (ctr !== smpState.counter) smpState.counter = ctr;
  const lk = readU32(c, SMP_LOCK);
  if (lk !== smpState.lock) smpState.lock = lk;
  if (smpState.msg[coreId] === 0) smpState.msg[coreId] = readU32(c, SMP_MSG + coreId * 4);
  smpState.park |= readU32(c, SMP_PARK);
}

function smpBoot(elf) {
  cores = [];
  entries = [elf.entry, 0, 0, 0];
  smpState = { go: 0, counter: 0, lock: 0, park: 0, msg: [0, 0, 0, 0], start: [0, 0, 0] };
  for (let i = 0; i < CORE_COUNT; i++) {
    const c = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
    c.mem_map(RAM_BASE, RAM_SIZE, ucMod.PROT_ALL);
    c.mem_map(Number(board.pi_uart_base()), UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
    c.mem_map(SMP_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
    c.devUart = { base: Number(board.pi_uart_base()) };
    // Each core prints through the PL011 DR: a TX write hook pushes the
    // chars (the cores have no slice-loop pump anymore — the slots are
    // gone).
    c.hook_add(
      ucMod.HOOK_MEM_WRITE,
      (u, access, addr, size, value) => {
        const b = Number(value) & 0xff;
        if (b !== 0) {
          stats.chars++;
          board.pi_cons_push(b);
        }
      },
      null,
      Number(board.pi_uart_base()),
      Number(board.pi_uart_base()) + 3
    );
    loadElf(c, elf);
    cores.push(c);
  }
}

// Round-robin scheduler over the 4 cores. Core 0 starts at e_entry and
// launches cores 1..3 by writing their entry addresses to START_ENTRY. A
// core is scheduled until it parks (sets its PARK_MASK bit) or all cores
// have parked and the console is drained.
function smpRun() {
  let out = '';
  const started = [true, false, false, false];
  const allParked = (1 << CORE_COUNT) - 1;
  for (let round = 0; round < MAX_ROUNDS; round++) {
    for (let i = 0; i < CORE_COUNT; i++) {
      if (!started[i]) {
        const e = smpState.start[i - 1];
        if (e === 0) continue;
        started[i] = true;
        entries[i] = e;
      }
      if (smpState.park & (1 << i)) continue;
      const c = cores[i];
      syncDeviceOut(c, i);
      const pc = Number(c.reg_read_i32(ucMod.ARM64_REG_PC)) || entries[i];
      const t0 = performance.now();
      c.emu_start(pc, 0, 0, SLICE_INSNS);
      stats.emuMs += performance.now() - t0;
      stats.steps += 1;
      stats.insns += SLICE_INSNS;
      syncDeviceIn(c, i);
    }
    out += drain(board);
    updateStats();
    if (smpState.park === allParked) {
      out += drain(board);
      break;
    }
  }
  if (smpState.park !== allParked) setStatus('warn: cores did not all park — check entry addresses');
  return out;
}

// MMU window: the host refreshes the status word before each slice (so the
// guest can read it back) and pulls the guest's enable/disable write after.
function syncMmuOut(uc) {
  if (mmuState) writeU32(uc, MMU_CTL, (mmuState.enabled ? 1 : 0) | mmuState.root);
}
function syncMmuIn(uc) {
  if (readU32(uc, MMU_DONE) !== 0) mmuDone = true;
  const v = readU32(uc, MMU_CTL);
  if (v === mmuCtl) return;
  mmuCtl = v;
  if (v & 1) {
    mmuState = mmuEnable(uc, ucMod, v & ~1);
    if (mmuHook) uc.hook_del(mmuHook);
    mmuHook = uc.hook_add(ucMod.HOOK_MEM_WRITE, (u, access, addr, size, value) => {
      mmuMirrorWrite(uc, mmuState, Number(addr), Number(size), value);
    });
  } else {
    if (mmuHook) {
      uc.hook_del(mmuHook);
      mmuHook = null;
    }
    mmuState = null;
  }
}

// DMA window: the host refreshes CS (END/INT flags) and ENABLE before each
// slice, then pulls the guest's writes out after it. An ACTIVE rise starts
// the chain (the host performs the transfers); the INT clear must be gated
// on the host having latched INT, or the same-slice ACTIVE write (which has
// no bit 4) would wipe a freshly latched dmaInt.
function syncDmaOut(uc) {
  let cs = 0;
  if (dmaEnd) cs |= 2;
  if (dmaInt) cs |= 4;
  writeU32(uc, DMA_CS, cs);
  dmaLastCS = cs;
  writeU32(uc, DMA_ENABLE, dmaEnable);
}
function syncDmaIn(uc) {
  const cs = readU32(uc, DMA_CS);
  const conblk = readU32(uc, DMA_CONBLK);
  dmaEnable = readU32(uc, DMA_ENABLE);
  if (readU32(uc, DMA_DONE) !== 0) dmaDone = true;
  if (cs & (1 << 31)) {
    dmaEnd = false;
    dmaInt = false;
  } else {
    if ((cs & 1) && !(dmaLastCS & 1) && conblk !== 0 && (dmaEnable & 1)) {
      const r = dmaRunChain(uc, conblk);
      dmaEnd = true;
      if (r.int) {
        dmaInt = true;
        dmaIntSeen = true;
      }
    }
    if (dmaInt && (dmaLastCS & 4) !== 0 && (cs & 4) === 0) dmaInt = false; // guest cleared INT
  }
}

// PWM audio playback: the host drains the guest's FIFO into a sample ring
// (src/pwm.js); a ScriptProcessor pulls the ring to the speakers at the
// context's rate. Created on the Run click (a user gesture), so the
// AudioContext is allowed to play.
function initAudio() {
  if (audioCtx) return;
  const AC = window.AudioContext || window.webkitAudioContext;
  if (!AC) return;
  audioCtx = new AC();
  audioRing = new Float32Array(1 << 18);
  const sp = audioCtx.createScriptProcessor(4096, 0, 1);
  sp.onaudioprocess = (e) => {
    const out = e.outputBuffer.getChannelData(0);
    for (let i = 0; i < out.length; i++) {
      out[i] = audioLen > 0 ? audioRing[(audioPos++) & (audioRing.length - 1)] * 0.3 : 0;
      if (audioLen > 0) audioLen--;
    }
  };
  sp.connect(audioCtx.destination);
  if (audioCtx.state === 'suspended') audioCtx.resume();
}
function audioPush(sampleWord) {
  if (!audioRing) return;
  const s16 = ((sampleWord & 0xffff) << 16) >> 16; // low 16 bits = signed sample
  if (audioLen < audioRing.length) {
    audioRing[(audioPos + audioLen) & (audioRing.length - 1)] = s16 / 32768;
    audioLen++;
  }
}

// UART1 chars reach the same terminal, tagged "[u1] " once per line (the
// probe replicates the same tagging; the guest's own output is unchanged).
// Newline chars are never tagged, so a CRLF pair stays adjacent and the
// terminal renders it as a single line break.
function uart1Emit(b) {
  const isNl = b === 0x0a || b === 0x0d;
  if (uart1LineStart && !isNl) {
    for (const c of '[u1] ') {
      stats.chars++;
      board.pi_cons_push(c.charCodeAt(0));
    }
    uart1LineStart = false;
  }
  stats.chars++;
  board.pi_cons_push(b);
  if (isNl) uart1LineStart = true;
}

function runSlice(count) {
  const pc = irqResume || irqVector || Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || uc.entry;
  if (irqResume) irqInFlight = false;
  irqResume = 0;
  irqVector = 0;
  syncTimerOut(uc);
  syncMailboxOut(uc);
  if (gpio) gpio.syncOut(uc);
  if (ic) ic.syncOut(uc);
  syncLocalOut(uc);
  if (mode === LIRQ_MODE || mode === LINUX_MODE) {
    // The arch timer counter runs at the real 19.2 MHz rate (CNTFRQ):
    // LIRQ_MODE ticks it so Phase A's CNTP_CTL compare can fire, and Linux
    // reads CNTPCT for timekeeping (the clockevent needs it to advance too).
    const us = (performance.now() - tmrWall0) * 1000;
    uc.arm64_timer_tick(Math.floor(us * 19.2));
  }
  syncMmuOut(uc);
  syncDmaOut(uc);
  if (uart0SyncOut) uart0SyncOut(uc);
  if (pwmSyncOut) pwmSyncOut(uc);
  if (i2cSyncOut) i2cSyncOut(uc);
  if (spiSyncOut) spiSyncOut(uc);
  if (sdSyncOut) sdSyncOut(uc);
  if (uart1SyncOut) uart1SyncOut(uc);
  const t0 = performance.now();
  uc.emu_start(pc, 0, 0, count);
  stats.emuMs += performance.now() - t0;
  stats.steps += 1;
  stats.insns += count;
  syncTimerIn(uc);
  syncMailboxIn(uc);
  if (gpio) {
    gpio.syncIn(uc);
    updateGpioPanel();
  }
  if (ic) ic.syncIn(uc);
  syncLocalIn(uc);
  syncIrqRet(uc);
  syncMmuIn(uc);
  syncDmaIn(uc);
  if (pwmSyncIn) {
    pwmSyncIn(uc);
    while (pwmAudioFed < pwmState.drained) audioPush(pwmState.ring[pwmAudioFed++]);
  }
  if (i2cSyncIn) i2cSyncIn(uc);
  if (spiSyncIn) spiSyncIn(uc);
  if (sdSyncIn) sdSyncIn(uc);
  if (uart0SyncIn) uart0SyncIn(uc);
  if (uart1SyncIn) uart1SyncIn(uc);
  const out = drain(board);
  irqDeliver(uc);
  return out;
}

function updateStats() {
  const wall = (performance.now() - stats.wallStart) / 1000;
  const mips = stats.emuMs > 0 ? (stats.insns / stats.emuMs / 1000).toFixed(2) : '—';
  let row = '';
  if (mode === SMP_MODE && cores) {
    for (let i = 0; i < CORE_COUNT; i++) {
      const c = cores[i];
      const pc = (Number(c.reg_read_i32(ucMod.ARM64_REG_PC)) || entries[i] || 0) - 0x100000;
      const sp = Number(c.reg_read_i32(ucMod.ARM64_REG_SP)) || 0;
      row += `<span><span class="k">c${i}</span> pc 0x${pc.toString(16).padStart(6, '0')} sp 0x${sp.toString(16)}</span>`;
    }
  } else {
    const pc = (Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || uc.entry) - 0x100000;
    const sp = Number(uc.reg_read_i32(ucMod.ARM64_REG_SP));
    row = `<span><span class="k">pc</span> 0x100000+0x${pc.toString(16).padStart(6, '0')}</span>` +
      `<span><span class="k">sp</span> 0x${sp.toString(16)}</span>`;
  }
  statsEl.innerHTML =
    row +
    `<span><span class="k">mips</span> ${mips}</span>` +
    `<span><span class="k">steps</span> ${stats.steps}</span>` +
    `<span><span class="k">insns</span> ${stats.insns}</span>` +
    `<span><span class="k">emu</span> ${stats.emuMs.toFixed(2)}ms</span>` +
    `<span><span class="k">wall</span> ${wall.toFixed(2)}s</span>` +
    `<span><span class="k">chars</span> ${stats.chars}</span>`;
}

// The clock and gpio guests sleep for real wall-clock time, which the idle
// heuristic can't tell apart from an infinite spin, so they use the same
// explicit-done protocol as smp: slices until the guest writes TMR_DONE.
function runUntilDone() {
  let out = '';
  clockDone = false;
  for (let i = 0; i < CLOCK_MAX_SLICES; i++) {
    const o = runSlice(SLICE_INSNS);
    out += o;
    updateStats();
    if (clockDone) {
      out += drain(board);
      break;
    }
  }
  if (!clockDone) setStatus('warn: guest did not reach DONE');
  return out;
}

// The mmu guest also has silent phases (building page tables, waiting for the
// host to enable translation), which would trip the idle heuristic, so it uses
// the same explicit-done protocol: slices until the guest writes MMU_DONE.
function runUntilMmuDone() {
  let out = '';
  mmuDone = false;
  for (let i = 0; i < MMU_MAX_SLICES; i++) {
    const o = runSlice(SLICE_INSNS);
    out += o;
    updateStats();
    if (mmuDone) {
      out += drain(board);
      break;
    }
  }
  if (!mmuDone) setStatus('warn: mmu guest did not reach DONE');
  return out;
}

// The dma guest finishes its chain work and verification in one burst after
// the host performs the transfers, so it uses the same explicit-done
// protocol: slices until the guest writes DMA_DONE.
function runUntilDmaDone() {
  let out = '';
  dmaDone = false;
  for (let i = 0; i < DMA_MAX_SLICES; i++) {
    const o = runSlice(SLICE_INSNS);
    out += o;
    updateStats();
    if (dmaDone) {
      out += drain(board);
      break;
    }
  }
  if (!dmaDone) setStatus('warn: dma guest did not reach DONE');
  return out;
}

// The pwm guest generates the whole tune (paced by the FIFO handshake) in
// one burst, so it uses the explicit-done protocol: slices until the guest
// writes PWM_DONE; the drained samples keep playing from the audio ring.
function runUntilPwmDone() {
  let out = '';
  for (let i = 0; i < PWM_MAX_SLICES && !pwmState.done; i++) {
    const o = runSlice(SLICE_INSNS);
    out += o;
    updateStats();
  }
  if (!pwmState.done) setStatus('warn: pwm guest did not reach DONE');
  else out += drain(board);
  return out;
}

// The i2c guest runs its three sensor transfers then parks, so it uses the
// explicit-done protocol: slices until the guest writes I2C_DONE.
function runUntilI2cDone() {
  let out = '';
  for (let i = 0; i < I2C_MAX_SLICES && !i2cState.done; i++) {
    const o = runSlice(SLICE_INSNS);
    out += o;
    updateStats();
  }
  if (!i2cState.done) setStatus('warn: i2c guest did not reach DONE');
  else out += drain(board);
  return out;
}

// The spi guest runs its two identical JEDEC transactions then parks, so it
// uses the explicit-done protocol: slices until the guest writes SPI_DONE.
function runUntilSpiDone() {
  let out = '';
  for (let i = 0; i < SPI_MAX_SLICES && !spiState.done; i++) {
    const o = runSlice(SLICE_INSNS);
    out += o;
    updateStats();
  }
  if (!spiState.done) setStatus('warn: spi guest did not reach DONE');
  else out += drain(board);
  return out;
}

// The sd guest reads the FAT12 card and prints HELLO.TXT, then parks, so it
// uses the explicit-done protocol: slices until the guest writes SD_DONE.
function runUntilSdDone() {
  let out = '';
  for (let i = 0; i < SD_MAX_SLICES && !sdState.done; i++) {
    const o = runSlice(SLICE_INSNS);
    out += o;
    updateStats();
  }
  if (!sdState.done) setStatus('warn: sd guest did not reach DONE');
  else out += drain(board);
  return out;
}

// The gpio chase is paced in animation frames: slices run for ~16 ms of wall
// time per frame, so the browser paints the LED panel between frames and the
// knight-rider chase is actually visible (the guest sleeps by the same wall
// clock, so emulated time keeps pace with the display).
let gpioLoopActive = false;
let gpioFrame = 0;
function gpioRun() {
  let out = '';
  clockDone = false;
  gpioLoopActive = true;
  const frame = () => {
    const t0 = performance.now();
    do {
      out += runSlice(SLICE_INSNS);
      if (clockDone) break;
    } while (performance.now() - t0 < 16);
    draw(out);
    out = '';
    updateStats();
    if (clockDone) {
      gpioLoopActive = false;
      setStatus('booted — running gpio — GPIO @ 0x3F200000 — chase done — hold BTN 29 to press');
      return;
    }
    gpioFrame = requestAnimationFrame(frame);
  };
  gpioFrame = requestAnimationFrame(frame);
}

// The fb guest animates forever and never parks, so a rAF loop advances
// slices (~16 ms of wall time per frame) and blits the framebuffer to the
// canvas right after each batch — the display runs live until Reboot.
function fbRun() {
  let out = '';
  const frame = () => {
    if (mode !== FB_MODE) return;
    const t0 = performance.now();
    do {
      out += runSlice(SLICE_INSNS);
    } while (performance.now() - t0 < 16);
    draw(out);
    out = '';
    updateStats();
    if (fbReady) blit();
    fbFrame = requestAnimationFrame(frame);
  };
  fbFrame = requestAnimationFrame(frame);
}

function blit() {
  if (!fbCtx || fbW === 0) return;
  const n = fbW * fbH * 4;
  const mem = uc.mem_read(FB_ADDR, n);
  const img = fbCtx.createImageData(fbW, fbH);
  img.data.set(mem);
  for (let i = 3; i < img.data.length; i += 4) img.data[i] = 255; // opaque
  fbCtx.putImageData(img, 0, 0);
}

// The irq and lirq guests never park (infinite spin with IRQs unmasked), so
// they run on rAF slices; deliveries happen at slice boundaries inside
// runSlice (host-assisted for irq, real CPU_INTERRUPT_HARD for lirq).
function irqRun() {
  let out = '';
  const frame = () => {
    if (mode !== IRQ_MODE && mode !== LIRQ_MODE) return;
    const t0 = performance.now();
    do {
      out += runSlice(SLICE_INSNS);
    } while (performance.now() - t0 < 16);
    draw(out);
    out = '';
    updateStats();
    irqFrame = requestAnimationFrame(frame);
  };
  irqFrame = requestAnimationFrame(frame);
}

// Linux never idles, so it runs with the same fixed-budget frame loop as
// the IRQ guests; IRQs are delivered natively (CPU_INTERRUPT_HARD via the
// local block, real vectors, real eret) with the arch timer ticked per
// slice, exactly like LIRQ_MODE. The boot runs until the user reboots.
// Linux boot uses the qemu-wasm engine (a self-contained page at
// /linux/index.html) instead of the unicorn core. We embed it in an iframe
// rather than driving it from JS: the page wires xterm to the emulated
// PL011 via xterm-pty and boots the raspi3ap machine (4x Cortex-A53, 512 MB)
// with the prebuilt kernel8.img + DTB + busybox rootfs.
function runLinux() {
  cancelAnimationFrame(gpioFrame);
  cancelAnimationFrame(fbFrame);
  cancelAnimationFrame(irqFrame);
  gpioLoopActive = false;
  runBtn.disabled = true;
  term.hidden = true;
  gpioPanel.hidden = true;
  fbCanvas.hidden = true;
  const linuxBoot = document.getElementById('linuxBoot');
  if (linuxBoot) { linuxBoot.hidden = false; linuxBoot.textContent = 'booting Linux…'; }
  let frame = document.getElementById('linuxframe');
  if (!frame) {
    frame = document.createElement('iframe');
    frame.id = 'linuxframe';
    frame.style.width = '100%';
    frame.style.height = '82vh';
    frame.style.border = '0';
    frame.style.background = '#111';
    document.querySelector('main').appendChild(frame);
  }
  const loaded = frame.src && frame.src.indexOf('linux/index.html') !== -1;
  if (loaded) {
    try { frame.contentWindow.location.reload(); } catch (e) { frame.src = './linux/index.html'; }
  } else {
    frame.src = './linux/index.html';
  }
  frame.hidden = false;
  setStatus('booting Linux — qemu-wasm raspi3ap (Pi 3 B+, 4× Cortex-A53, 512 MB) — serial console in the frame below');
  hint.textContent = 'Linux runs in the embedded frame (cross-origin isolated for threads). Press Reboot to reload the VM.';
  runBtn.textContent = 'Reboot';
  runBtn.disabled = false;
}

// Receive boot-phase updates from the Linux iframe (serial milestones) and
// surface them in the parent UI's #linuxBoot indicator.
window.addEventListener('message', (e) => {
  if (e.data && e.data.type === 'linux-boot') {
    const el = document.getElementById('linuxBoot');
    if (el && !el.hidden) el.textContent = 'Linux boot: ' + e.data.phase;
  }
});

// The guest drives itself: it prints to the UART TX slots (one char per
// slice) and parks in getc until a key arrives. Run slices until the guest
// has gone quiet for two consecutive slices — i.e. it is back waiting for
// input (or finished all its work).
function runUntilIdle() {
  let out = '';
  let quiet = 0;
  for (let i = 0; i < MAX_SLICES; i++) {
    const o = runSlice(SLICE_INSNS);
    out += o;
    updateStats();
    if (o === '') {
      quiet++;
      if (quiet >= 2) break;
    } else {
      quiet = 0;
    }
  }
  return out;
}

function guestKey(code) {
  uart0Push(code); // queue into the PL011 RX FIFO (guest's getc pops it)
  return runUntilIdle();
}

function handleKey(e) {
  if (!uc || runBtn.disabled) return;
  if (mode === IRQ_MODE) {
    const c = e.key.length === 1 ? e.key.charCodeAt(0) : e.key === 'Enter' ? 13 : 0;
    if (!c) return;
    e.preventDefault();
    uart0Push(c); // RX FIFO -> RXINTR (IRQ 57) at the next slice
    return;
  }
  if (mode === SMP_MODE || mode === FB_MODE) return;
  if (e.key === 'Backspace') {
    e.preventDefault();
    if (term.textContent.length > 0) {
      term.textContent = term.textContent.slice(0, -1);
    }
    draw(guestKey(0x7f)); // guest unwrites its own line buffer
    return;
  }
  const c = e.key.length === 1 ? e.key.charCodeAt(0) : e.key === 'Enter' ? 13 : 0;
  if (!c) return;
  e.preventDefault(); // also stops the browser re-clicking a focused button on Enter
  draw(guestKey(c));
}

// On-screen keyboard: feed the same guestKey path as physical keys.
function tapKeys(btn) {
  if (!uc || runBtn.disabled) return;
  if (mode === IRQ_MODE) {
    const action = btn.dataset.action;
    if (action === 'enter') {
      uart0Push(13);
    } else if (action === 'bs') {
      uart0Push(0x7f);
    } else {
      for (const ch of btn.dataset.keys) uart0Push(ch.charCodeAt(0));
    }
    term.focus();
    return;
  }
  if (mode === SMP_MODE || mode === FB_MODE) return;
  const action = btn.dataset.action;
  if (action === 'enter') {
    draw(guestKey(13));
  } else if (action === 'bs') {
    if (term.textContent.length > 0) term.textContent = term.textContent.slice(0, -1);
    draw(guestKey(0x7f));
  } else {
    for (const ch of btn.dataset.keys) draw(guestKey(ch.charCodeAt(0)));
  }
  term.focus();
}

async function run() {
  // Linux boot is handled by the qemu-wasm engine in an embedded iframe,
  // not by the unicorn core — bail out before initializing unicorn.
  if (progSel.value === LINUX_MODE) {
    runLinux();
    return;
  }
  cancelAnimationFrame(gpioFrame);
  cancelAnimationFrame(fbFrame);
  cancelAnimationFrame(irqFrame);
  gpioLoopActive = false;
  runBtn.disabled = true;
  term.hidden = false;
  const linuxFrameEl = document.getElementById('linuxframe');
  if (linuxFrameEl) linuxFrameEl.hidden = true;
  const linuxBootEl = document.getElementById('linuxBoot');
  if (linuxBootEl) linuxBootEl.hidden = true;
  term.textContent = '';
  gpioPanel.hidden = true;
  fbCanvas.hidden = true;
  stats = { steps: 0, insns: 0, emuMs: 0, chars: 0, wallStart: performance.now() };
  statsEl.textContent = '';
  try {
    const MUnicorn = window.MUnicorn;
    if (!MUnicorn) throw new Error('unicorn.js failed to load (check public/unicorn.js)');
    ucMod = await MUnicorn();
    board = await loadBoard();
    uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);

    const name = PROGRAMS[progSel.value];
    const resp = await fetch('./programs/' + name);
    if (!resp.ok) throw new Error('cannot fetch ./programs/' + name);
    const elf = parseElf(new Uint8Array(await resp.arrayBuffer()));

    if (progSel.value === SMP_MODE) {
      mode = SMP_MODE;
      smpBoot(elf);
      draw(smpRun()); // 4 cores round-robin until all park
      setStatus(
        `booted — running smp — 4 AArch64 cores, mailbox @ 0x3F202000 — press Reboot to re-run`
      );
    } else if (progSel.value === CLOCK_MODE) {
      mode = CLOCK_MODE;
      boot(ucMod, uc, board, elf);
      draw(runUntilDone()); // runs until the guest writes TMR_DONE
      setStatus(
        `booted — running clock — BCM system timer @ 0x3F003000 — press Reboot to re-run`
      );
    } else if (progSel.value === GPIO_MODE) {
      mode = GPIO_MODE;
      gpioPanel.hidden = false;
      boot(ucMod, uc, board, elf);
      updateGpioPanel();
      gpioRun(); // async: rAF-paced slices, chase visibly blinks the LEDs
      setStatus(`booted — running gpio — GPIO @ 0x3F200000 — chase in progress`);
    } else if (progSel.value === FB_MODE) {
      mode = FB_MODE;
      fbCanvas.hidden = false;
      boot(ucMod, uc, board, elf);
      fbRun(); // async: rAF-paced slices + canvas blit every frame
      setStatus(
        `booted — running fb — framebuffer 160x120x32 @ 0x200000 via mailbox — live canvas`
      );
    } else if (progSel.value === IRQ_MODE || progSel.value === 'uart0') {
      mode = IRQ_MODE;
      boot(ucMod, uc, board, elf);
      irqRun(); // async: rAF-paced slices, IRQs delivered at slice ends
      setStatus(
        progSel.value === 'uart0'
          ? `booted — running uart0 — BCM2837 PL011 @ 0x3F201000 — config verified, RXINTR -> IRQ 57 — type a key`
          : `booted — running irq — BCM2835 interrupt controller @ 0x3F00B200 — timer + PL011 IRQs live`
      );
    } else if (progSel.value === LIRQ_MODE) {
      mode = LIRQ_MODE;
      boot(ucMod, uc, board, elf);
      irqRun(); // async: rAF-paced slices, real IRQs via the local block
      setStatus(
        `booted — running lirq — BCM2836 local interrupt block @ 0x40000000 — CNTPNS + GPU IRQ delivered to a real vector`
      );
    } else if (progSel.value === 'mmu') {
      mode = 'mmu';
      boot(ucMod, uc, board, elf);
      draw(runUntilMmuDone()); // guest builds page tables, host walks them on MMU_CTL
      setStatus(
        `booted — running mmu — host-assisted MMU @ 0x3F00D000 — press Reboot to re-run`
      );
    } else if (progSel.value === 'dma') {
      mode = 'dma';
      boot(ucMod, uc, board, elf);
      draw(runUntilDmaDone()); // host performs the 3-CB chain between slices
      setStatus(
        `booted — running dma — BCM2835 DMA @ 0x3F007000 — 3-CB chain + completion IRQ — press Reboot to re-run`
      );
    } else if (progSel.value === 'pwm') {
      mode = 'pwm';
      boot(ucMod, uc, board, elf);
      initAudio(); // the Run click is a user gesture: the melody can play
      draw(runUntilPwmDone()); // FIFO-mode sample generation, paced by FULL1
      setStatus(
        `booted — running pwm — BCM2835 PWM @ 0x3F20C000 — ${pwmState.drained} samples in the audio ring — press Reboot to re-run`
      );
    } else if (progSel.value === 'i2c') {
      mode = 'i2c';
      boot(ucMod, uc, board, elf);
      draw(runUntilI2cDone()); // sensor slave: WHO_AM_I, TEMP, COUNTER reads
      setStatus(
        `booted — running i2c — BCM2835 BSC master @ 0x3F804000 — sensor reads — press Reboot to re-run`
      );
    } else if (progSel.value === 'spi') {
      mode = 'spi';
      boot(ucMod, uc, board, elf);
      draw(runUntilSpiDone()); // flash slave answers the JEDEC ID, twice
      setStatus(
        `booted — running spi — BCM2835 SPI0 master @ 0x3F204000 — JEDEC ID — press Reboot to re-run`
      );
    } else if (progSel.value === 'uart1') {
      mode = 'uart1';
      boot(ucMod, uc, board, elf);
      draw(runUntilIdle()); // second console: parks on getc like the shell
      setStatus(
        `booted — running uart1 — BCM2835 AUX mini UART @ 0x3F215000 — output tagged [u1] — press Reboot to re-run`
      );
    } else if (progSel.value === 'sd') {
      mode = 'sd';
      boot(ucMod, uc, board, elf);
      draw(runUntilSdDone()); // FAT12 card: boot sector, root dir, HELLO.TXT
      setStatus(
        `booted — running sd — BCM2835 SDHCI (EMMC) @ 0x3F300000 — FAT12 card, HELLO.TXT read — press Reboot to re-run`
      );
    } else {
      mode = 'single';
      boot(ucMod, uc, board, elf);
      draw(runUntilIdle()); // program boots, prints its banner, parks on getc
      setStatus(`booted — running ${name} (AArch64 ELF at 0x100000) — type, or press Reboot`);
    }
    runBtn.textContent = 'Reboot';
    runBtn.disabled = false;
    term.focus();
    hint.textContent = '';
  } catch (err) {
    setStatus('ERROR: ' + (err && (err.stack || err.message || err) || err));
    console.error(err);
    runBtn.disabled = false;
  }
}

// The GPIO button is a host-side input: while held, the host drives BTN 29
// high in GPLEV, and slices are resumed so the guest's poll loop sees it.
// (During the rAF-paced chase the frame loop advances the guest itself.)
function pressGpioBtn(down) {
  if (!uc || runBtn.disabled || mode !== GPIO_MODE) return;
  gpioBtn = down ? 1 : 0;
  gpioBtnEl.classList.toggle('held', !!down);
  // The button level reaches the guest at the next slice; re-arm the local
  // block line in case the level change set a GPIO event bit.
  rearmGpuLine(uc);
  if (!gpioLoopActive) draw(runUntilIdle());
}

window.addEventListener('keydown', handleKey);
term.addEventListener('click', () => term.focus());
document.querySelectorAll('.osk button').forEach((btn) =>
  btn.addEventListener('click', () => tapKeys(btn))
);
gpioBtnEl.addEventListener('pointerdown', () => pressGpioBtn(true));
gpioBtnEl.addEventListener('pointerup', () => pressGpioBtn(false));
gpioBtnEl.addEventListener('pointerleave', () => pressGpioBtn(false));
window.addEventListener('error', (e) => {
  setStatus('ERROR: ' + (e.message || e.type));
});
runBtn.addEventListener('click', run);
run();