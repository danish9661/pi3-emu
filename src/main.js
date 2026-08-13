import './styles.css';
import { parseElf, loadElf } from './elf.js';
import { mmuEnable, mmuMirrorWrite } from './mmu.js';
import { dmaRunChain } from './dma.js';

const UART_WINDOW = 0x1000;
const TX_SLOT_STRIDE = 4;
const TX_SLOTS = 32;
const RAM_BASE = 0x0;
const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const MAX_SLICES = 5000;

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

// BCM2837 GPIO (real layout). Writes are host-arbitrated like the timer:
// the host pulls GPSET/GPCLR out of the window after each slice to track
// output levels, and refreshes GPLEV before each slice so input pins show
// the UI button. The 8 LEDs live on pins 21..28, the button on pin 29.
const GPIO_BASE = 0x3F200000;
const GPIO_WINDOW = GPIO_BASE; // already 4K-aligned
const GPSET0 = GPIO_BASE + 0x1C;
const GPCLR0 = GPIO_BASE + 0x28;
const GPLEV0 = GPIO_BASE + 0x34;
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
const IRQ_DMA0 = 1 << 16;
const DMA_MAX_SLICES = 30000;
let dmaInt = false;
let dmaEnd = false;
let dmaEnable = 0;
let dmaLastCS = 0;
let dmaDone = false;
let dmaIntSeen = false;

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
let rxSlot = 0;
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

let gpioOut = 0; // guest-driven output levels (built from GPSET/GPCLR)
let gpioBtn = 0; // host-driven input: button pin high while held
let gpioLedEls = null; // DOM spans for the LED panel

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
function boot(ucMod, uc, board, elf) {
  const uart = Number(board.pi_uart_base());
  rxSlot = uart + Number(board.pi_rx_offset());

  uc.mem_map(RAM_BASE, RAM_SIZE, ucMod.PROT_ALL);
  uc.mem_map(uart, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(TMR_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(MBOX_WINDOW, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(GPIO_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(MMU_CTL, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(DMA_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(0x3F00E000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.devUart = { base: uart };
  uc.entry = elf.entry;
  tmrWall0 = performance.now();
  tmrPending = 0;
  tmrCrossed = [false, false, false, false];
  tmrCompares = [0, 0, 0, 0];
  tmrLastCS = 0;
  mbxLastWrite = 0;
  mbxPending = false;
  mbxAddr = 0;
  gpioOut = 0;
  gpioBtn = 0;
  fbW = 0;
  fbH = 0;
  fbDepth = 0;
  fbPitch = 0;
  fbReady = false;
  icEnabled1 = 0;
  icEnabled2 = 0;
  icEnabledBasic = 0;
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

  loadElf(uc, elf);
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

// ---- BCM2835 interrupt controller (0x3F00B200): the "basic" pending
// register and pending 1 share the same 32 lines; we model pending-1 with
// the timer (bit 29) and PL011 UART (bit 31) sources driven by host state,
// plus enable/disable masks the guest writes. ----

const IC_BASE = 0x3F00B200;
const IC_PENDING_BASIC = IC_BASE + 0x00;
const IC_PENDING1 = IC_BASE + 0x04;
const IC_PENDING2 = IC_BASE + 0x08;
const IC_ENABLE_IRQS1 = IC_BASE + 0x10;
const IC_ENABLE_IRQS2 = IC_BASE + 0x14;
const IC_ENABLE_BASIC = IC_BASE + 0x18;
const IC_DISABLE_IRQS1 = IC_BASE + 0x1C;
const IC_DISABLE_IRQS2 = IC_BASE + 0x20;
const IC_DISABLE_BASIC = IC_BASE + 0x24;
const IC_IRQ_TIMER1 = 1 << 29; // system timer compare 1
const IC_IRQ_UART = 1 << 31; // PL011 UART

let icEnabled1 = 0;
let icEnabled2 = 0;
let icEnabledBasic = 0;

// Pending lines are derived from the device state before every slice, and
// enable/disable writes are pulled out after it (write-mask semantics, like
// the other host-arbitrated windows).
function syncIcOut(uc) {
  let p1 = 0;
  if (tmrPending & 4) p1 |= IC_IRQ_TIMER1; // CS bit 2 = the irq guest's C1 (0x3014)
  if (uc.mem_read(rxSlot, 1)[0] !== 0) p1 |= IC_IRQ_UART; // RX byte waiting
  if (dmaInt && (dmaEnable & 1)) p1 |= IRQ_DMA0; // DMA channel 0 completion
  writeU32(uc, IC_PENDING_BASIC, p1);
  writeU32(uc, IC_PENDING1, p1);
  writeU32(uc, IC_PENDING2, 0);
}

function syncIcIn(uc) {
  const en = readU32(uc, IC_ENABLE_IRQS1);
  const dis = readU32(uc, IC_DISABLE_IRQS1);
  if (en) writeU32(uc, IC_ENABLE_IRQS1, 0);
  if (dis) writeU32(uc, IC_DISABLE_IRQS1, 0);
  icEnabled1 = (icEnabled1 | en) & ~dis;
  const en2 = readU32(uc, IC_ENABLE_IRQS2);
  const dis2 = readU32(uc, IC_DISABLE_IRQS2);
  if (en2) writeU32(uc, IC_ENABLE_IRQS2, 0);
  if (dis2) writeU32(uc, IC_DISABLE_IRQS2, 0);
  icEnabled2 = (icEnabled2 | en2) & ~dis2;
  const enB = readU32(uc, IC_ENABLE_BASIC);
  const disB = readU32(uc, IC_DISABLE_BASIC);
  if (enB) writeU32(uc, IC_ENABLE_BASIC, 0);
  if (disB) writeU32(uc, IC_DISABLE_BASIC, 0);
  icEnabledBasic = (icEnabledBasic | enB) & ~disB;
}

// Deliver a pending, enabled IRQ at a slice boundary. reg_write(PC/ELR) is a
// no-op in this unicorn build, so the delivery is host-assisted: the next
// slice *starts* at the IRQ vector (VBAR + 0x280), and the vector stub
// signals IRQ_RET when the handler is done — the host then resumes the guest
// at the saved PC. While a handler is in flight no further delivery happens
// (the DAIF.I mask is not observable through this build's PSTATE).
const IC_IRQ_RET = IC_BASE + 0x2C;
let irqElr = 0;
let irqInFlight = false;
let irqVector = 0;
let irqResume = 0;

function irqDeliver(uc) {
  let pending = readU32(uc, IC_PENDING1) & icEnabled1;
  // The PENDING1 window is refreshed before each slice, so it can still hold
  // the DMA0 bit after the guest has cleared CS.INT. Re-derive the line from
  // host state: once INT is cleared, a stale window must not re-deliver.
  if (!(dmaInt && (dmaEnable & 1))) pending &= ~IRQ_DMA0;
  if (!pending || irqInFlight) return;
  if (mode === SMP_MODE) return;
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

// GPIO: refresh input levels (GPLEV = guest outputs + host button) before a
// slice, then pull GPSET/GPCLR out after it. GPSET/GPCLR stay set in the
// window (real semantics: write-1 registers), so level tracking is monotone:
// a bit set in GPSET latches the output high, a bit set in GPCLR latches low.
function syncGpioOut(uc) {
  const host = gpioBtn ? (1 << GPIO_BTN) : 0;
  writeU32(uc, GPLEV0, (gpioOut & ~(1 << GPIO_BTN)) | host);
  writeU32(uc, GPLEV0 + 4, 0); // pins 32..53 stay low
}

function syncGpioIn(uc) {
  const set = readU32(uc, GPSET0);
  const clr = readU32(uc, GPCLR0);
  gpioOut = (gpioOut | set) & ~clr;
  updateGpioPanel();
}

// The 8 LED dots mirror the guest-driven levels (red = on).
function updateGpioPanel() {
  if (mode !== GPIO_MODE || !gpioPanel) return;
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
  for (let i = 0; i < GPIO_LEDS.length; i++) {
    gpioLedEls[i].classList.toggle('on', (gpioOut & (1 << GPIO_LEDS[i])) !== 0);
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

function pumpUart(ucMod, uc, board) {
  const { base } = uc.devUart;
  const window = uc.mem_read(base, TX_SLOTS * TX_SLOT_STRIDE);
  let found = 0;
  for (let i = 0; i < TX_SLOTS; i++) {
    const c = window[i * TX_SLOT_STRIDE];
    if (c !== 0) {
      found++;
      stats.chars++;
      board.pi_cons_push(c);
      for (let k = 0; k < TX_SLOT_STRIDE; k++) {
        uc.mem_write(base + i * TX_SLOT_STRIDE + k, [0]);
      }
    }
  }
  return found;
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
      pumpUart(ucMod, c, board);
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

function runSlice(count) {
  const pc = irqResume || irqVector || Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || uc.entry;
  if (irqResume) irqInFlight = false;
  irqResume = 0;
  irqVector = 0;
  syncTimerOut(uc);
  syncMailboxOut(uc);
  syncGpioOut(uc);
  syncIcOut(uc);
  syncMmuOut(uc);
  syncDmaOut(uc);
  const t0 = performance.now();
  uc.emu_start(pc, 0, 0, count);
  stats.emuMs += performance.now() - t0;
  stats.steps += 1;
  stats.insns += count;
  syncTimerIn(uc);
  syncMailboxIn(uc);
  syncGpioIn(uc);
  syncIcIn(uc);
  syncIrqRet(uc);
  syncMmuIn(uc);
  syncDmaIn(uc);
  pumpUart(ucMod, uc, board);
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

// The irq guest also never parks (infinite spin with IRQs unmasked), so it
// runs on rAF slices; deliveries happen at slice boundaries inside runSlice.
function irqRun() {
  let out = '';
  const frame = () => {
    if (mode !== IRQ_MODE) return;
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
  uc.mem_write(rxSlot, [code]);
  return runUntilIdle();
}

function handleKey(e) {
  if (!uc || runBtn.disabled) return;
  if (mode === IRQ_MODE) {
    const c = e.key.length === 1 ? e.key.charCodeAt(0) : e.key === 'Enter' ? 13 : 0;
    if (!c) return;
    e.preventDefault();
    uc.mem_write(rxSlot, [c]); // rx slot -> UART RX IRQ at the next slice
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
      uc.mem_write(rxSlot, [13]);
    } else if (action === 'bs') {
      uc.mem_write(rxSlot, [0x7f]);
    } else {
      for (const ch of btn.dataset.keys) uc.mem_write(rxSlot, [ch.charCodeAt(0)]);
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
  cancelAnimationFrame(gpioFrame);
  cancelAnimationFrame(fbFrame);
  cancelAnimationFrame(irqFrame);
  gpioLoopActive = false;
  runBtn.disabled = true;
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
    } else if (progSel.value === IRQ_MODE) {
      mode = IRQ_MODE;
      boot(ucMod, uc, board, elf);
      irqRun(); // async: rAF-paced slices, IRQs delivered at slice ends
      setStatus(
        `booted — running irq — BCM2835 interrupt controller @ 0x3F00B200 — timer + UART IRQs live`
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