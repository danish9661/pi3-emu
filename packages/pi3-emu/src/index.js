// pi3-emu core — headless Raspberry Pi 3 (BCM2837) emulator for node and
// browsers (no DOM required). This is the rp2040js-style engine half of the
// pi3-emu project: an AArch64 CPU (unicorn.js, vendored) plus JS device
// models with real BCM2837 register layouts, driven in bounded slices with
// host-arbitrated MMIO sync before/after each slice.
//
// Minimal use (node):
//   import { Pi3Emulator, loadUnicorn } from 'pi3-emu';
//   import { readFileSync } from 'node:fs';
//   const ucMod = await loadUnicorn();
//   const emu = new Pi3Emulator(ucMod);
//   await emu.loadFirmware(readFileSync('./firmware/shell.elf'));
//   emu.runUntilIdle();                       // boot banner + prompt
//   emu.sendLine('hi'); emu.runUntilIdle();   // -> HELLO
//   console.log(emu.consoleText);
//
// Browser use: load vendor/unicorn.js with a <script> tag for the global
// MUnicorn, then `const ucMod = await window.MUnicorn()` and pass it in.
// Firmware can come from anywhere (fetch/upload); see examples/.

import { parseElf, loadElf } from './elf.js';
import { readU32, writeU32 } from './perf.js';
import { createUart0 } from './uart0.js';
import { createUart1 } from './uart1.js';
import { createGpio } from './gpio.js';
import { createIc } from './ic.js';
import { createLocalInt } from './localint.js';
import { createI2c } from './i2c.js';
import { createSpi } from './spi.js';
import { createPwm } from './pwm.js';
import { createSdhci } from './sdhci.js';
import { createRng } from './rng.js';
import { createTempSensor } from './temp.js';
import { createClockMgr } from './clockmgr.js';
import { createI2s } from './i2s.js';
import { createSpi1 } from './spi1.js';
import { createSpi2 } from './spi2.js';
import { createUart25 } from './uart25.js';
import { createUsb } from './usb.js';
import { mmuEnable, mmuMirrorWrite } from './mmu.js';
import { dmaRunChain } from './dma.js';
import { decodeFault } from './fault.js';

export { parseElf, loadElf, readU32, writeU32 };
export { createUart0, createUart1, createGpio, createIc, createLocalInt };
export { createI2c, createSpi, createPwm, createSdhci };
export { createRng, createTempSensor, createClockMgr, createI2s };
export { createSpi1, createSpi2, createUart25, createUsb };
export { mmuEnable, mmuMirrorWrite, dmaRunChain };
export { decodeFault };

// Load the vendored unicorn.js CPU core (node path; browsers use the
// <script> global MUnicorn instead and pass the module in directly).
export async function loadUnicorn() {
  const { createRequire } = await import('node:module');
  const { fileURLToPath } = await import('node:url');
  const { dirname, join } = await import('node:path');
  const require = createRequire(import.meta.url);
  const here = dirname(fileURLToPath(import.meta.url));
  return require(join(here, '..', 'vendor', 'unicorn.js'))();
}

export const RAM_BASE = 0x0;
export const RAM_SIZE = 0x400000;
export const UART_WINDOW = 0x1000;
export const SLICE_INSNS = 4096;
export const MAX_SLICES = 5000;
// Default virtual-time rate: ~measured throughput of the rebuilt core, so a
// virtual second costs about a wall second on a typical machine (but stays
// exact regardless of host speed).
export const VIRTUAL_IPS = 10000000;

export const UART0_BASE = 0x3f201000; // PL011 console
export const TMR_BASE = 0x3f003000; // system timer
export const TMR_CS = TMR_BASE + 0x00;
export const TMR_CLO = TMR_BASE + 0x04;
export const TMR_CMP = TMR_BASE + 0x0c;
export const TMR_DONE = TMR_BASE + 0x20;
export const GPIO_BASE = 0x3f200000;
export const GPIO_LEDS = [21, 22, 23, 24, 25, 26, 27, 28];
export const GPIO_BTN = 29;
export const IC_BASE = 0x3f00b200;
export const IC_IRQ_RET = IC_BASE + 0x2c;
export const LOCAL_BASE = 0x40000000;
export const MBOX_WINDOW = 0x3f00b000; // mailbox lives at +0x880 in this page
export const MMU_CTL = 0x3f00d000;
export const MMU_DONE = MMU_CTL + 0x04;
export const DMA_BASE = 0x3f007000;
export const DMA_CS = DMA_BASE + 0x00;
export const DMA_CONBLK = DMA_BASE + 0x04;
export const DMA_ENABLE = 0x3f00e050;
export const DMA_DONE = 0x3f00e054;
export const PWM_BASE = 0x3f20c000;
export const I2C_BASE = 0x3f804000;
export const SPI_BASE = 0x3f204000;
export const UART1_BASE = 0x3f215000;
export const SD_BASE = 0x3f300000;
// Host extension: SD-card presence flag for boot.py auto-mount. Lives in
// the always-mapped mailbox window (mailbox regs sit at +0x880, the IC at
// +0x200 — +0xFF0 collides with neither). Driven every slice: 1 while an
// SDHCI card is attached, 0 otherwise. Reading it can never abort, unlike
// touching SD_BASE with no card mapped.
export const SD_PRESENT = MBOX_WINDOW + 0xFF0;

export class Pi3Emulator {
  // ucMod: unicorn module (await loadUnicorn() or window.MUnicorn()).
  // opts: { ramSize, sliceInsns, maxSlices, onConsole(text), onBridgeData(msg),
  //         realIrq, virtualTime } — realIrq enables native CPU_INTERRUPT_HARD
  //         delivery (lirq-style guests); default is host-assisted IRQ_RET
  //         delivery. virtualTime (true or { ips }) decouples the system timer
  //         from the wall clock: each slice advances the clock by
  //         insns/ips seconds, making runs deterministic and sleeps instant.
  constructor(ucMod, opts = {}) {
    this.ucMod = ucMod;
    this.ramSize = opts.ramSize || RAM_SIZE;
    this.sliceInsns = opts.sliceInsns || SLICE_INSNS;
    this.maxSlices = opts.maxSlices || MAX_SLICES;
    this.onConsole = opts.onConsole || null;
    this.onBridgeData = opts.onBridgeData || null;
    this.realIrq = !!opts.realIrq;
    this.virtualIps = opts.virtualTime
      ? (typeof opts.virtualTime === 'object' && opts.virtualTime.ips) || VIRTUAL_IPS
      : 0;
    this.virtualUs = 0;
    this.uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
    this.entry = 0;
    this.consoleText = '';
    this.stats = { steps: 0, insns: 0 };
    this.lastError = null;
    this.lastFault = null;
    this.lastSyncError = null;
    this.faultStreak = 0;
    this.devices = []; // generic { syncOut?, syncIn? } synced every slice
    // Device state (timers, irq machinery).
    this.tmrWall0 = Date.now();
    this.tmrPending = 0;
    this.tmrCrossed = [false, false, false, false];
    this.tmrCompares = [0, 0, 0, 0];
    this.tmrLastCS = 0;
    this.irqElr = 0;
    this.irqInFlight = false;
    this.irqVector = 0;
    this.irqResume = 0;
    this.gpioBtn = 0;
    this.mmuState = null;
    this.mmuHook = null;
    this.mmuCtl = 0;
    this.dmaInt = false;
    this.dmaEnd = false;
    this.dmaEnable = 0;
    this.dmaLastCS = 0;
    this.pwmAudioFed = 0;
    this.uart1LineStart = true;

    const uc = this.uc;
    uc.mem_map(RAM_BASE, this.ramSize, ucMod.PROT_ALL);
    uc.mem_map(UART0_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
    uc.mem_map(TMR_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
    uc.mem_map(GPIO_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
    // IC_BASE (0x3F00B200) is NOT 4K-aligned so it cannot be mapped itself;
    // it rides inside the MBOX_WINDOW page (0x3F00B000), like the main app.
    uc.mem_map(LOCAL_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
    // Mailbox window is zero-mapped (no VideoCore model in core v1): guests
    // that issue mailbox tags will spin on STATUS; run loops stay bounded.
    uc.mem_map(MBOX_WINDOW, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);

    // PL011 console: TX bytes feed the console, RX bytes come from pushKey.
    const uart0 = createUart0(uc, ucMod, UART0_BASE, (b) => this.emit(b),
      () => this.rearmGpuLine());
    this.uart0SyncOut = uart0.syncOut;
    this.uart0SyncIn = uart0.syncIn;
    this.uart0Push = uart0.push;
    this.uart0IrqActive = uart0.irqActive;

    this.gpio = createGpio(uc, ucMod, GPIO_BASE, {
      getBtn: () => this.gpioBtn << GPIO_BTN,
      onIrqChange: () => this.rearmGpuLine(),
    });
    this.ic = createIc(uc, ucMod, IC_BASE, () => this.icLines());
    this.localInt = createLocalInt(uc, ucMod, LOCAL_BASE, () => this.localLines());
    uc.hook_add(
      ucMod.HOOK_MEM_WRITE,
      (u, access, addr, size, value) => this.tmrCsHook(Number(value)),
      null, TMR_CS, TMR_CS + 3
    );
  }

  // A device mirror write must never kill the host loop (e.g. a test that
  // unmaps a window): record the first failure and continue best-effort.
  safeSync(fn) {
    try {
      fn();
    } catch (e) {
      if (!this.lastSyncError) this.lastSyncError = e;
    }
  }
  // Emulated "now" in microseconds: wall clock, or the virtual counter.
  emuNowUs() {
    if (this.virtualIps) return Math.floor(this.virtualUs);
    return (Date.now() - this.tmrWall0) * 1000;
  }
  // ---- small helpers -------------------------------------------------
  dbg(sel) {
    try { return Number(this.uc.arm64_debug(sel)); } catch (_) { return 0; }
  }
  hasDebug() {
    try { return typeof this.uc.arm64_debug === 'function'; } catch (_) { return false; }
  }
  emit(b) {
    this.consoleText += String.fromCharCode(b & 0xff);
  }
  pushKey(code) {
    this.uart0Push(code & 0xff);
  }
  sendLine(s) {
    for (const ch of String(s)) this.pushKey(ch.charCodeAt(0));
    this.pushKey(13);
  }
  setButton(down) {
    this.gpioBtn = down ? 1 : 0;
    this.rearmGpuLine();
  }
  ledLevels() {
    const out = this.gpio.state.out;
    return GPIO_LEDS.map((p) => ((out >>> p) & 1) === 1);
  }
  readU32(addr) {
    return readU32(this.uc, addr);
  }

  // ---- interrupt machinery (ported from the browser host loop) --------
  icLines() {
    return {
      timer: this.tmrPending & 0xf,
      dma0: this.dmaInt && (this.dmaEnable & 1) !== 0,
      pl011: this.uart0IrqActive ? this.uart0IrqActive() : false,
      sdhci: this.sdIrqActive ? this.sdIrqActive() : false,
      gpio0: this.gpio ? this.gpio.irqActive(0) : false,
      gpio1: this.gpio ? this.gpio.irqActive(1) : false,
      aux: false,
    };
  }
  localLines() {
    return {
      cntps: this.dbg(13) ? 1 : 0,
      cntpns: this.dbg(3) ? 1 : 0,
      cnthp: this.dbg(12) ? 1 : 0,
      cntv: this.dbg(11) ? 1 : 0,
      gpu: this.ic ? this.ic.line() : 0,
      pmu: 0,
      axi: 0,
      ltimer: 0,
      mailbox: [0, 0, 0, 0],
    };
  }
  syncLocalOut() {
    if (!this.localInt) return;
    this.localInt.syncOut(this.uc);
    if (this.realIrq) {
      try { this.localInt.syncIrq(this.uc, (level) => this.uc.arm64_set_irq(level)); } catch (_) {}
    }
  }
  syncLocalIn() {
    if (this.localInt) this.localInt.syncIn(this.uc);
  }
  rearmGpuLine() {
    if (this.localInt && this.realIrq) {
      try { this.localInt.syncIrq(this.uc, (level) => this.uc.arm64_set_irq(level)); } catch (_) {}
    }
  }
  tmrCsHook(value) {
    this.tmrPending &= value & 0xf;
    this.rearmGpuLine();
  }
  daifI() {
    return ((this.dbg(1) >> 7) & 1) === 1;
  }
  irqDeliver() {
    if (this.irqInFlight || this.realIrq) return;
    if (!this.ic || !this.hasDebug() || this.daifI()) return;
    const p = this.ic.pending();
    if ((p.b1 | p.b2 | p.basic) === 0) return;
    const ucMod = this.ucMod;
    this.irqElr = Number(this.uc.reg_read_i32(ucMod.ARM64_REG_PC)) || this.entry;
    this.irqInFlight = true;
    const vbar = Number(this.uc.reg_read_i32(ucMod.ARM64_REG_VBAR_EL1)) || 0x100000;
    this.irqVector = vbar + 0x280;
  }
  syncIrqRet() {
    if (readU32(this.uc, IC_IRQ_RET) !== 0) {
      writeU32(this.uc, IC_IRQ_RET, 0);
      this.irqResume = this.irqElr;
    }
  }

  // ---- system timer ---------------------------------------------------
  syncTimerOut() {
    const us = this.emuNowUs() & 0xffffffff;
    writeU32(this.uc, TMR_CLO, us);
    writeU32(this.uc, TMR_CLO + 4, 0);
    for (let i = 0; i < 4; i++) {
      const c = this.tmrCompares[i];
      if (!this.tmrCrossed[i] && c !== 0 && ((us - c) & 0x80000000) === 0) {
        this.tmrCrossed[i] = true;
        this.tmrPending |= 1 << i;
      }
    }
    writeU32(this.uc, TMR_CS, this.tmrPending);
    this.tmrLastCS = this.tmrPending;
  }
  syncTimerIn() {
    for (let i = 0; i < 4; i++) {
      const c = readU32(this.uc, TMR_CMP + i * 4);
      if (c !== this.tmrCompares[i]) this.tmrCrossed[i] = false;
      this.tmrCompares[i] = c;
    }
    const cs = readU32(this.uc, TMR_CS);
    if (cs !== this.tmrLastCS) this.tmrPending &= cs & 0xf;
  }

  // ---- generic device attach ------------------------------------------
  // Any { syncOut?, syncIn? } object joins the per-slice sync.
  attach(dev) {
    this.devices.push(dev);
    return dev;
  }
  mapWindow(base, size = UART_WINDOW) {
    this.uc.mem_map(base, size, this.ucMod.PROT_READ | this.ucMod.PROT_WRITE);
  }
  attachUart1() {
    this.mapWindow(UART1_BASE);
    const u = createUart1(this.uc, this.ucMod, UART1_BASE, (b) => this.uart1Emit(b));
    return this.attach(u);
  }
  uart1Emit(b) {
    const isNl = b === 0x0a || b === 0x0d;
    if (this.uart1LineStart && !isNl) {
      for (const c of '[u1] ') this.emit(c.charCodeAt(0));
      this.uart1LineStart = false;
    }
    this.emit(b);
    if (isNl) this.uart1LineStart = true;
  }
  attachI2c(base = I2C_BASE, onBridgeData) {
    this.mapWindow(base);
    return this.attach(createI2c(this.uc, this.ucMod, base, onBridgeData || this.onBridgeData));
  }
  attachSpi(base = SPI_BASE, onBridgeData) {
    this.mapWindow(base);
    return this.attach(createSpi(this.uc, this.ucMod, base, onBridgeData || this.onBridgeData));
  }
  attachPwm(onSamples) {
    this.mapWindow(PWM_BASE);
    const pwm = createPwm(this.uc, this.ucMod, PWM_BASE, this.onBridgeData);
    const fed = { n: 0 };
    const innerIn = pwm.syncIn.bind(pwm);
    const self = this;
    return this.attach({
      state: pwm.state,
      syncOut: pwm.syncOut,
      syncIn(uc) {
        innerIn(uc);
        if (onSamples) {
          while (fed.n < pwm.state.drained) onSamples(pwm.state.ring[fed.n++]);
        }
      },
    });
  }
  attachSdhci() {
    this.mapWindow(SD_BASE);
    const sd = createSdhci(this.uc, this.ucMod, SD_BASE, () => this.rearmGpuLine());
    this.sdIrqActive = sd.irqActive;
    this.sd = sd;
    return this.attach(sd);
  }
  // SD card snapshot: flat sector image for save/restore across sessions
  // (download/upload the bytes; importCard before boot, or remount after —
  // a live VfsFat mount caches sectors, see sdcard.write docs).
  exportCard() {
    return this.sd ? this.sd.exportImage() : null;
  }
  importCard(bytes) {
    return this.sd ? this.sd.loadImage(bytes) : false;
  }
  attachMmu() {
    this.mapWindow(MMU_CTL);
    const self = this;
    return this.attach({
      syncOut(uc) {
        if (self.mmuState) writeU32(uc, MMU_CTL, (self.mmuState.enabled ? 1 : 0) | self.mmuState.root);
      },
      syncIn(uc) {
        const v = readU32(uc, MMU_CTL);
        if (v === self.mmuCtl) return;
        self.mmuCtl = v;
        if (v & 1) {
          self.mmuState = mmuEnable(uc, self.ucMod, v & ~1);
          if (self.mmuHook) uc.hook_del(self.mmuHook);
          self.mmuHook = uc.hook_add(self.ucMod.HOOK_MEM_WRITE, (u, access, addr, size, value) => {
            mmuMirrorWrite(uc, self.mmuState, Number(addr), Number(size), value);
          });
        } else {
          if (self.mmuHook) { uc.hook_del(self.mmuHook); self.mmuHook = null; }
          self.mmuState = null;
        }
      },
    });
  }
  attachDma() {
    this.mapWindow(DMA_BASE);
    this.mapWindow(DMA_ENABLE & ~0xfff);
    const self = this;
    return this.attach({
      syncOut(uc) {
        let cs = 0;
        if (self.dmaEnd) cs |= 2;
        if (self.dmaInt) cs |= 4;
        writeU32(uc, DMA_CS, cs);
        self.dmaLastCS = cs;
        writeU32(uc, DMA_ENABLE, self.dmaEnable);
      },
      syncIn(uc) {
        const cs = readU32(uc, DMA_CS);
        const conblk = readU32(uc, DMA_CONBLK);
        self.dmaEnable = readU32(uc, DMA_ENABLE);
        if (cs & (1 << 31)) {
          self.dmaEnd = false;
          self.dmaInt = false;
        } else {
          if ((cs & 1) && !(self.dmaLastCS & 1) && conblk !== 0 && (self.dmaEnable & 1)) {
            const r = dmaRunChain(uc, conblk);
            self.dmaEnd = true;
            if (r.int) self.dmaInt = true;
          }
          if (self.dmaInt && (self.dmaLastCS & 4) !== 0 && (cs & 4) === 0) self.dmaInt = false;
        }
      },
    });
  }

  // ---- firmware + scheduler --------------------------------------------
  loadFirmware(bytes) {
    const elf = parseElf(new Uint8Array(bytes));
    this.entry = loadElf(this.uc, elf);
    this.tmrWall0 = Date.now();
    this.virtualUs = 0;
    return this.entry;
  }
  runSlice(count) {
    const ucMod = this.ucMod;
    const uc = this.uc;
    const n = count || this.sliceInsns;
    const pc = this.irqResume || this.irqVector || Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || this.entry;
    if (this.irqResume) this.irqInFlight = false;
    this.irqResume = 0;
    this.irqVector = 0;
    this.syncTimerOut();
    this.safeSync(() => writeU32(uc, SD_PRESENT, this.sdIrqActive ? 1 : 0));
    this.safeSync(() => this.gpio.syncOut(uc));
    this.safeSync(() => this.ic.syncOut(uc));
    this.safeSync(() => this.syncLocalOut());
    if (this.realIrq && this.hasDebug()) {
      try {
        const tv = BigInt(Math.floor(this.emuNowUs() * 19.2));
        if (process.env.LIRQTRACE) console.error(`tick vus=${this.virtualUs} tv=${tv}`);
        uc.arm64_timer_tick(tv);
      } catch (_) {}
    }
    if (this.uart0SyncOut) this.safeSync(() => this.uart0SyncOut(uc));
    for (const d of this.devices) { if (d.syncOut) this.safeSync(() => d.syncOut(uc)); }
    if (process.env.LIRQTRACE) console.error(`start pc=0x${pc.toString(16)} n=${n} gt0pre=${this.dbg(3)}`);
    try {
      uc.emu_start(pc, 0, 0, n);
      this.faultStreak = 0;
    } catch (e) {
      // Unmapped accesses (e.g. unmodeled windows) fault naturally; decode
      // for humans, then resume from the faulting PC like the browser loop.
      this.lastError = e;
      try {
        this.lastFault = decodeFault(uc, ucMod, e,
          { ramBase: RAM_BASE, ramSize: this.ramSize });
      } catch (_) {
        this.lastFault = null;
      }
      this.faultStreak++;
    }
    this.stats.steps += 1;
    this.stats.insns += n;
    if (this.virtualIps) this.virtualUs += (n / this.virtualIps) * 1e6;
    this.syncTimerIn();
    this.safeSync(() => this.gpio.syncIn(uc));
    this.safeSync(() => this.ic.syncIn(uc));
    this.safeSync(() => this.syncLocalIn());
    this.syncIrqRet();
    if (this.uart0SyncIn) this.safeSync(() => this.uart0SyncIn(uc));
    for (const d of this.devices) { if (d.syncIn) this.safeSync(() => d.syncIn(uc)); }
    this.irqDeliver();
  }
  // Drain newly emitted console text since the last call.
  takeConsole() {
    const out = this.consoleText.slice(this.consoleDrained || 0);
    this.consoleDrained = this.consoleText.length;
    if (out && this.onConsole) this.onConsole(out);
    return out;
  }
  // Run until the guest goes quiet (parks polling for input) or the budget
  // runs out. Returns the console output produced.
  runUntilIdle(maxSlices) {
    let out = '';
    let quiet = 0;
    const budget = maxSlices || this.maxSlices;
    for (let i = 0; i < budget; i++) {
      const before = this.consoleText.length;
      this.runSlice();
      out += this.takeConsole();
      if (this.consoleText.length === before) {
        quiet++;
        if (quiet >= 2) break;
      } else {
        quiet = 0;
      }
    }
    return out;
  }
  // Run until poll() is true (explicit-done guests: clock/gpio TMR_DONE,
  // mmu/dma/sd DONE protocols, i2c/spi state.done, ...).
  runUntilDone(poll, maxSlices) {
    let out = '';
    const budget = maxSlices || this.maxSlices;
    for (let i = 0; i < budget; i++) {
      this.runSlice();
      out += this.takeConsole();
      if (poll()) {
        this.runSlice();
        out += this.takeConsole();
        break;
      }
    }
    return out;
  }
}
