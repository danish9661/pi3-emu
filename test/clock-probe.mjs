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
const PROG = join(__dirname, '..', 'public', 'programs', 'clock.elf');

const TMR_BASE = 0x3f003000;
const TMR_CS = TMR_BASE;
const TMR_CLO = TMR_BASE + 4;
const TMR_CMP = TMR_BASE + 0x0c;
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
  uc.devUart = { base: uart };
  loadElf(uc, elf);

  const wall0 = Date.now();
  let pending = 0;
  let lastCS = 0;
  const crossed = [false, false, false, false];
  const compares = [0, 0, 0, 0];
  let done = false;
  let chars = '';

  const syncOut = () => {
    const us = ((Date.now() - wall0) * 1000) & 0xffffffff;
    writeU32(uc, TMR_CLO, us);
    writeU32(uc, TMR_CLO + 4, 0);
    for (let i = 0; i < 4; i++) {
      const c = compares[i];
      if (!crossed[i] && c !== 0 && ((us - c) & 0x80000000) === 0) {
        crossed[i] = true;
        pending |= 1 << i;
      }
    }
    writeU32(uc, TMR_CS, pending);
    lastCS = pending;
  };
  const syncIn = () => {
    for (let i = 0; i < 4; i++) compares[i] = readU32(uc, TMR_CMP + i * 4);
    const cs = readU32(uc, TMR_CS);
    if (cs !== lastCS) pending &= cs & 0xf; // guest rewrote the status mask
    if (readU32(uc, TMR_DONE) !== 0) done = true;
  };
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

  const t0 = Date.now();
  for (let i = 0; i < MAX_SLICES; i++) {
    syncOut();
    const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || elf.entry;
    uart0.syncOut(uc);
    uc.emu_start(pc, 0, 0, SLICE_INSNS);
    syncIn();
    uart0.syncIn(uc);
    chars += drain();
    if (done) {
      chars += drain();
      console.log('clock run: slices', i, '| wall', Date.now() - t0, 'ms');
      break;
    }
  }

  console.log('console output:', JSON.stringify(chars));
  const checks = ['boot: CLO = ', 'sleeping 1 s', 'elapsed ', 'C1 match (M1) = 1', 'CS after clear = 0', 'done'];
  for (const w of checks) console.log('contains:', JSON.stringify(w), '->', chars.includes(w));
  const m = chars.match(/elapsed (\d+)/);
  if (m) {
    const el = Number(m[1]);
    console.log('elapsed us:', el, '-> plausibly ~1s:', el > 700000 && el < 1500000);
  }
  console.log('guest signalled DONE:', done);
  uc.close();
}

main().then(() => process.exit(0)).catch((e) => {
  console.error('FATAL', String(e && e.message || e).slice(0, 300));
  process.exit(1);
});