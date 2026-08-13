import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));
const { createUart0 } = await import(join(__dirname, '..', 'src', 'uart0.js'));
const { createPwm } = await import(join(__dirname, '..', 'src', 'pwm.js'));

const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const UART_WINDOW = 0x1000;
const PROG = join(__dirname, '..', 'public', 'programs', 'pwm.elf');

const PWM_BASE = 0x3f20c000;

const ucMod = await MUnicorn();
const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
uc.mem_map(0x3f201000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(PWM_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
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
function writeU32(addr, v) {
  uc.mem_write(
    addr,
    new Uint8Array([v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff])
  );
}
const uart0 = createUart0(uc, ucMod, uart, (b) => board.pi_cons_push(b));

function drain() {
  let out = '';
  for (;;) {
    const c = Number(board.pi_cons_poll());
    if (c === -1 || c === 0xffffffff) break;
    out += String.fromCharCode(c);
  }
  return out;
}

const pwm = createPwm(uc, ucMod, PWM_BASE);
const { state, syncOut, syncIn } = pwm;

function slice() {
  const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || uc.entry;
  syncOut(uc);
  uart0.syncOut(uc);
  uc.emu_start(pc, 0, 0, SLICE_INSNS);
  syncIn(uc);
  uart0.syncIn(uc);
  chars += drain();
}

uc.hook_add(ucMod.HOOK_CODE, (u, a) => {});
for (let i = 0; i < 30000 && !state.done; i++) slice();

const H = 0x5fff;
const L = 0xffffa000;
const ring = state.ring;
const expectedTotal = 5292 * 12 + 10584 * 2; // 12 notes x 120ms + 2 whole notes
const firstLow = ring[0] === H;
let firstHigh = -1;
for (let i = 0; i < ring.length; i++) {
  if (ring[i] === L) {
    firstHigh = i;
    break;
  }
}
const waveHasBoth = firstHigh > 0 && ring.every((v) => v === H || v === L);
let emptySeen = false;
// The guest waits for EMPT1 before writing DONE: the FIFO must be empty
// when it parked, so every pushed sample reached the ring.
const fifoEmpty = state.fifo.length === 0;

const noteLines = (chars.match(/pwm: note \d+ Hz/g) || []).length;
const want = {
  banner: chars.includes('pwm: BCM2835 PWM controller @ 0x3F20C000'),
  fifoMode: chars.includes('pwm: FIFO mode enabled (USEF1|MSEN1), FIFO cleared'),
  ctlLatched: (state.ctl & 0xa3) === 0xa3, // PWEN1|MODE1|USEF1|MSEN1
  notes: noteLines === 14,
  firstNote: chars.includes('pwm: note 262 Hz'),
  lastNote: chars.includes('pwm: note 294 Hz'),
  played: chars.includes('pwm: all notes played (84672 samples)'),
  parked: chars.includes('pwm: parked'),
  hostDone: state.done,
  count: ring.length === expectedTotal,
  drained: state.drained === expectedTotal,
  firstSampleLow: firstLow,
  waveHasBoth,
  highHalfCount: ring.filter((v) => v === H).length > 0,
  fifoEmpty,
};

console.log('pwm-probe:');
for (const [k, v] of Object.entries(want)) {
  console.log('  ' + (v ? 'ok ' : 'FAIL') + ' ' + k + (v ? '' : ' (want)'));
}
const pass = Object.values(want).every(Boolean);
console.log(
  pass
    ? 'pwm-probe: PASS (ring=' + ring.length + ', first-high@' + firstHigh + ')'
    : 'pwm-probe: FAIL'
);
if (!pass) {
  console.log('--- guest output ---');
  console.log(chars.replace(/\r/g, ''));
}
uc.close();
process.exit(pass ? 0 : 1);
