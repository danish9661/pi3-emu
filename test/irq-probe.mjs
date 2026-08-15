import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));
const { createUart0 } = await import(join(__dirname, '..', 'src', 'uart0.js'));
const { createIc } = await import(join(__dirname, '..', 'src', 'ic.js'));

const UART_WINDOW = 0x1000;
const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const PROG = join(__dirname, '..', 'public', 'programs', 'irq.elf');

const TMR_BASE = 0x3f003000;
const TMR_CS = TMR_BASE + 0x00;
const TMR_CLO = TMR_BASE + 0x04;
const TMR_CMP = TMR_BASE + 0x0c;

const IC_BASE = 0x3f00b200;
const IC_IRQ_RET = IC_BASE + 0x2c;

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}
function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}

async function main() {
  const ucMod = await MUnicorn();
  const board = (
    await WebAssembly.instantiate(
      readFileSync(join(__dirname, '..', 'public', 'pi_board.wasm')),
      {}
    )
  ).instance.exports;
  const uart = Number(board.pi_uart_base());
  const elf = parseElf(new Uint8Array(readFileSync(PROG)));

  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
  uc.mem_map(uart, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(TMR_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(0x3f00b000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.devUart = { base: uart };
  const uart0 = createUart0(uc, ucMod, uart, (b) => board.pi_cons_push(b));
  loadElf(uc, elf);

  const wall0 = Date.now();
  let tmrPending = 0;
  let tmrCompares = [0, 0, 0, 0];
  let tmrCrossed = [false, false, false, false];
  let tmrLastCS = 0;
  let chars = '';
  let delivered = 0;
  let irqElr = 0;
  let irqInFlight = false;
  let irqVector = 0;
  let irqResume = 0;

  const syncTmrOut = () => {
    const us = ((Date.now() - wall0) * 1000) & 0xffffffff;
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
  };
  const syncTmrIn = () => {
    for (let i = 0; i < 4; i++) {
      const c = readU32(uc, TMR_CMP + i * 4);
      if (c !== tmrCompares[i]) tmrCrossed[i] = false;
      tmrCompares[i] = c;
    }
    const cs = readU32(uc, TMR_CS);
    if (cs !== tmrLastCS) tmrPending &= cs & 0xf;
  };
  // The real 3-bank legacy IC (src/ic.js), fed by the probe's host state.
  const ic = createIc(uc, ucMod, IC_BASE, () => ({
    timer: tmrPending & 0xf,
    dma0: false,
    pl011: uart0.irqActive(),
    sdhci: false,
    gpio0: false,
    gpio1: false,
    aux: false,
  }));
  const syncIrqRet = () => {
    const r = readU32(uc, IC_IRQ_RET);
    if (r !== 0) {
      writeU32(uc, IC_IRQ_RET, 0);
      irqResume = irqElr;
    }
  };
  const irqDeliver = () => {
    if (irqInFlight) return;
    if (((Number(uc.arm64_debug(1)) >> 7) & 1) === 1) return; // DAIF.I set
    // ic.pending() is derived fresh from the device lines on every call.
    const p = ic.pending();
    if ((p.b1 | p.b2 | p.basic) === 0) return;
    const pcNow = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC));
    irqElr = pcNow || elf.entry;
    irqInFlight = true;
    const vbar = Number(uc.reg_read_i32(ucMod.ARM64_REG_VBAR_EL1)) || 0x100000;
    irqVector = vbar + 0x280;
    delivered++;
  };
  let keySlices2 = 0;
  const drain = () => {
    let out = '';
    for (;;) {
      const ch = Number(board.pi_cons_poll());
      if (ch === -1 || ch === 0xffffffff) break;
      out += String.fromCharCode(ch);
    }
    return out;
  };
  let keySlices = 0;
  const slice = () => {
    const pc = irqResume || irqVector || Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || elf.entry;
    if (irqResume) irqInFlight = false;
    irqResume = 0;
    irqVector = 0;
    syncTmrOut();
    ic.syncOut(uc);
    uart0.syncOut(uc);
    uc.emu_start(pc, 0, 0, SLICE_INSNS);
    syncTmrIn();
    ic.syncIn(uc);
    syncIrqRet();
    uart0.syncIn(uc);
    const out = drain();
    irqDeliver();
    return out;
  };

  // Phase 1: boot + arm. The timer IRQ should fire at ~1s wall time.
  const t0 = Date.now();
  for (let i = 0; i < 30000 && !chars.includes('[irq #1 '); i++) chars += slice();

  // Phase 2: send a key -> UART RX IRQ.
  uart0.push(0x48); // 'H' into the PL011 RX FIFO
  for (let i = 0; i < 30000 && !chars.includes("[irq key: 'H']"); i++) {
    chars += slice();
  }

  const irqCount = (chars.match(/\[irq #(\d+)/g) || []).length;
  const ticks = (chars.match(/\[irq #(\d+) t\+1s\]/g) || []).length;
  const keySeen = chars.includes("[irq key: 'H']");
  const checks = [
    ['interrupt controller banner:', chars.includes('irq: BCM2835 interrupt controller')],
    ['timer + UART armed:', chars.includes('irq: timer C1 (IRQ 1) + UART RX (IRQ 57) armed')],
    ['timer IRQ delivered:', ticks >= 1],
    ['UART RX IRQ delivered:', keySeen],
    ['deliveries happened:', delivered >= 1],
  ];
  for (const [name, ok] of checks) console.log(ok ? 'PASS' : 'FAIL', '-', name);
  console.log('irq ticks seen:', ticks, '| irq lines:', irqCount, '| deliveries:', delivered);
  console.log('console output:', JSON.stringify(chars));
  console.log('session time:', Date.now() - t0, 'ms');
  uc.close();
  process.exit(checks.every(([, ok]) => ok) ? 0 : 1);
}

main().catch((e) => {
  console.error('FATAL', String(e && e.message || e).slice(0, 300));
  process.exit(1);
});