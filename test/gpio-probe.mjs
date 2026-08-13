import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));
const { createUart0 } = await import(join(__dirname, '..', 'src', 'uart0.js'));

const UART_WINDOW = 0x1000;
const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const MAX_SLICES = 60000;
const PROG = join(__dirname, '..', 'public', 'programs', 'gpio.elf');

const GPIO_BASE = 0x3f200000;
const GPSET0 = GPIO_BASE + 0x1c;
const GPCLR0 = GPIO_BASE + 0x28;
const GPLEV0 = GPIO_BASE + 0x34;
const LEDS = [21, 22, 23, 24, 25, 26, 27, 28];
const BTN = 29;

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
  uc.devUart = { base: uart };
  loadElf(uc, elf);

  const wall0 = Date.now();
  let gpioOut = 0;
  let btn = 0;
  let toggles = 0;
  let done = false;
  let chars = '';

  const syncGpioOut = () => {
    writeU32(uc, GPLEV0, (gpioOut & ~(1 << BTN)) | (btn ? 1 << BTN : 0));
    writeU32(uc, GPLEV0 + 4, 0);
  };
  const syncGpioIn = () => {
    const set = readU32(uc, GPSET0);
    const clr = readU32(uc, GPCLR0);
    const next = (gpioOut | set) & ~clr;
    if (next !== gpioOut) toggles++;
    gpioOut = next;
  };
  const syncTmr = () => writeU32(uc, TMR_CLO, ((Date.now() - wall0) * 1000) & 0xffffffff);
  const uart0 = createUart0(uc, ucMod, uc.devUart.base, (b) => board.pi_cons_push(b));
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
    syncTmr();
    syncGpioOut();
    const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || elf.entry;
    uart0.syncOut(uc);
    uc.emu_start(pc, 0, 0, SLICE_INSNS);
    syncGpioIn();
    if (readU32(uc, TMR_DONE) !== 0) done = true;
    uart0.syncIn(uc);
    return drain();
  };

  // Phase 1: chase — run until the guest writes TMR_DONE (parks on BTN 29).
  const t0 = Date.now();
  for (let i = 0; i < MAX_SLICES && !done; i++) chars += slice();
  const chaseSlices = chars.length && done ? chars.split('chase:')[1] || '' : '';
  console.log('chase: done =', done, '| slices->', 'ok');
  console.log('LED level transitions observed:', toggles);

  // Phase 2: press, release, press again — each press edge must be reported.
  let presses = 0;
  let btnState = 1; // start held
  const count = () => (chars.match(/button: (\d+) pressed/g) || []).length;
  let stable = 0;
  while (presses < 2 && Date.now() - t0 < 20000) {
    btn = btnState;
    chars += slice();
    const n = count();
    if (n > presses) {
      presses = n;
      stable = 0;
    }
    if (presses >= 2) break;
    // release once the press is reported, then press again after a breather
    if (btnState === 1) {
      if (presses === 1 && stable++ > 3) btnState = 0;
      else if (presses === 0 && stable++ > 3 && readU32(uc, TMR_DONE) !== 0) btnState = 0;
    } else {
      if (stable++ > 6) btnState = 1;
    }
  }

  console.log('console output:', JSON.stringify(chars));
  const checks = [
    'gpio: LEDs 21..28 output',
    'chase:',
    'chase done',
    'button: 1 pressed',
    'button: 2 pressed',
  ];
  for (const w of checks) console.log('contains:', JSON.stringify(w), '->', chars.includes(w));
  console.log('LED toggles >= 8:', toggles >= 8);
  console.log('session time:', Date.now() - t0, 'ms');
  uc.close();
}

main().then(() => process.exit(0)).catch((e) => {
  console.error('FATAL', String(e && e.message || e).slice(0, 300));
  process.exit(1);
});