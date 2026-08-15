import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));
const { createUart0 } = await import(join(__dirname, '..', 'src', 'uart0.js'));
const { createGpio } = await import(join(__dirname, '..', 'src', 'gpio.js'));
const { createIc } = await import(join(__dirname, '..', 'src', 'ic.js'));

const UART_WINDOW = 0x1000;
const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const MAX_SLICES = 60000;
const PROG = join(__dirname, '..', 'public', 'programs', 'gpio.elf');

const GPIO_BASE = 0x3f200000;
const LEDS = [21, 22, 23, 24, 25, 26, 27, 28];
const BTN = 29;

const IC_BASE = 0x3f00b200;
const IC_IRQ_RET = IC_BASE + 0x2c;

const TMR_BASE = 0x3f003000;
const TMR_CLO = TMR_BASE + 4;
const TMR_DONE = TMR_BASE + 0x20;

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
  uc.mem_map(GPIO_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(0x3f00b000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE); // legacy IC
  uc.devUart = { base: uart };
  loadElf(uc, elf);

  const wall0 = Date.now();
  let btn = 0;
  let toggles = 0;
  let done = false;
  let chars = '';
  let irqElr = 0;
  let irqInFlight = false;
  let irqVector = 0;
  let irqResume = 0;
  let delivered = 0;

  // The real GPIO model: edges are detected host-side at slice boundaries.
  const gpio = createGpio(uc, ucMod, GPIO_BASE, {
    getBtn: () => btn << BTN,
    onIrqChange: () => {},
  });
  const ic = createIc(uc, ucMod, IC_BASE, () => ({
    timer: 0,
    dma0: false,
    pl011: false,
    sdhci: false,
    gpio0: gpio.irqActive(0),
    gpio1: gpio.irqActive(1),
    aux: false,
  }));
  const syncTmr = () => writeU32(uc, TMR_CLO, ((Date.now() - wall0) * 1000) & 0xffffffff);
  const uart0 = createUart0(uc, ucMod, uc.devUart.base, (b) => board.pi_cons_push(b));
  const syncIrqRet = () => {
    const r = readU32(uc, IC_IRQ_RET);
    if (r !== 0) {
      writeU32(uc, IC_IRQ_RET, 0);
      irqResume = irqElr;
    }
  };
  const drain = () => {
    let out = '';
    for (;;) {
      const ch = Number(board.pi_cons_poll());
      if (ch === -1 || ch === 0xffffffff) break;
      out += String.fromCharCode(ch);
    }
    return out;
  };
  const slice = () => {
    const pc = irqResume || irqVector || Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || elf.entry;
    if (irqResume) irqInFlight = false;
    irqResume = 0;
    irqVector = 0;
    syncTmr();
    gpio.syncOut(uc);
    ic.syncOut(uc);
    uart0.syncOut(uc);
    uc.emu_start(pc, 0, 0, SLICE_INSNS);
    const before = gpio.state.out;
    gpio.syncIn(uc);
    if (gpio.state.out !== before) toggles++;
    ic.syncIn(uc);
    syncIrqRet();
    if (readU32(uc, TMR_DONE) !== 0) done = true;
    uart0.syncIn(uc);
    const out = drain();
    // Host-assisted IRQ delivery (same as the irq guest path).
    if (!irqInFlight && ((Number(uc.arm64_debug(1)) >> 7) & 1) === 0) {
      const p = ic.pending();
      if ((p.b1 | p.b2 | p.basic) !== 0) {
        irqElr = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || elf.entry;
        irqInFlight = true;
        const vbar = Number(uc.reg_read_i32(ucMod.ARM64_REG_VBAR_EL1)) || 0x100000;
        irqVector = vbar + 0x280;
        delivered++;
      }
    }
    return out;
  };

  // Phase 1: chase — run until the guest writes TMR_DONE (parks on BTN 29).
  const t0 = Date.now();
  for (let i = 0; i < MAX_SLICES && !done; i++) chars += slice();
  console.log('chase: done =', done, '| slices->', 'ok');
  console.log('LED level transitions observed:', toggles);

  // Phase 2: press, release, press again — press 1 is polled, press 2 is
  // the rising edge that must fire the GPIO bank-0 IRQ (IRQ 81).
  let presses = 0;
  let btnState = 1; // start held
  const count = () => (chars.match(/button: 1 pressed/g) || []).length;
  let stable = 0;
  while (presses < 1 && Date.now() - t0 < 20000) {
    btn = btnState;
    chars += slice();
    const n = count();
    if (n > presses) {
      presses = n;
      stable = 0;
    }
    if (presses >= 1) break;
    // release once the press is reported, then press again after a breather
    if (btnState === 1) {
      if (stable++ > 3 && readU32(uc, TMR_DONE) !== 0) btnState = 0;
    } else {
      if (stable++ > 6) btnState = 1;
    }
  }
  // The guest has armed GPREN + IRQ 81 by now; release then press again.
  btn = 0;
  for (let i = 0; i < 4; i++) chars += slice();
  btn = 1;
  const irqT0 = Date.now();
  while (!chars.includes('gpio: IRQ on BTN 29') && Date.now() - irqT0 < 20000) {
    chars += slice();
  }

  console.log('console output:', JSON.stringify(chars));
  const checks = [
    'gpio: LEDs 21..28 output',
    'chase:',
    'chase done',
    'button: 1 pressed',
    'gpio: GPREN armed on BTN 29 -> IRQ 81',
    'gpio: IRQ on BTN 29',
    'gpio: IRQ phase done',
  ];
  for (const w of checks) console.log('contains:', JSON.stringify(w), '->', chars.includes(w));
  console.log('IRQ deliveries:', delivered, '| LED toggles >= 8:', toggles >= 8);
  console.log('session time:', Date.now() - t0, 'ms');
  uc.close();
  process.exit(
    checks.every((w) => chars.includes(w)) && delivered >= 1 ? 0 : 1
  );
}

main().then(() => process.exit(0)).catch((e) => {
  console.error('FATAL', String(e && e.message || e).slice(0, 300));
  process.exit(1);
});