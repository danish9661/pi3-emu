import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));
const { createUart1 } = await import(join(__dirname, '..', 'src', 'uart1.js'));

const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const UART_WINDOW = 0x1000;
const TX_SLOT_STRIDE = 4;
const TX_SLOTS = 32;
const PROG = join(__dirname, '..', 'public', 'programs', 'uart1.elf');

const AUX_BASE = 0x3f215000;

const ucMod = await MUnicorn();
const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
uc.mem_map(0x3f201000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(AUX_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
const elf = parseElf(new Uint8Array(readFileSync(PROG)));
uc.entry = loadElf(uc, elf);

const board = (
  await WebAssembly.instantiate(
    readFileSync(join(__dirname, '..', 'public', 'pi_board.wasm')),
    {}
  )
).instance.exports;
const uart = Number(board.pi_uart_base());

let chars = '';
let u1 = ''; // UART1 stream, tagged like the browser console
let lineStart = true;
const PREFIX = '[u1] ';
function pump() {
  let out = '';
  const win = uc.mem_read(uart, TX_SLOTS * TX_SLOT_STRIDE);
  for (let i = 0; i < TX_SLOTS; i++) {
    for (let k = 0; k < TX_SLOT_STRIDE; k++) {
      const c = win[i * TX_SLOT_STRIDE + k];
      if (c) {
        out += String.fromCharCode(c);
        uc.mem_write(uart + i * TX_SLOT_STRIDE + k, [0]);
      }
    }
  }
  return out;
}
function uart1Emit(c) {
  const isNl = c === 0x0a || c === 0x0d;
  if (lineStart && !isNl) {
    u1 += PREFIX;
    lineStart = false;
  }
  u1 += String.fromCharCode(c);
  if (isNl) lineStart = true;
}

const uart1 = createUart1(uc, ucMod, AUX_BASE, uart1Emit);
const { state, syncOut, syncIn } = uart1;

function slice() {
  const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || uc.entry;
  syncOut(uc);
  uc.emu_start(pc, 0, 0, SLICE_INSNS);
  chars += pump();
  syncIn(uc);
}

for (let i = 0; i < 30000; i++) {
  slice();
  if (chars.includes('uart1: parked') && i > 2000) break;
}

const want = {
  banner0: chars.includes('uart1: mini UART (AUX) @ 0x3F215000'),
  consoleActive: chars.includes('uart1: UART0 console is active'),
  started: chars.includes('uart1: starting UART1 diagnostics'),
  diag1: u1.includes('[u1] uart1: hello from the mini UART'),
  diag2: u1.includes('[u1] uart1: LSR TX-empty pacing works'),
  diag3: u1.includes('[u1] uart1: diag line 3/3'),
  complete: chars.includes('uart1: UART1 diagnostics complete'),
  parked: chars.includes('uart1: parked'),
  enabled: state.enabled,
  noTagOnUart0: !chars.includes('[u1]'),
};

console.log('uart1-probe:');
for (const [k, v] of Object.entries(want)) {
  console.log('  ' + (v ? 'ok ' : 'FAIL') + ' ' + k + (v ? '' : ' (want)'));
}
const pass = Object.values(want).every(Boolean);
console.log(pass ? 'uart1-probe: PASS' : 'uart1-probe: FAIL');
if (!pass) {
  console.log('--- UART0 output ---');
  console.log(chars.replace(/\r/g, ''));
  console.log('--- UART1 output ---');
  console.log(u1.replace(/\r/g, ''));
}
uc.close();
process.exit(pass ? 0 : 1);
