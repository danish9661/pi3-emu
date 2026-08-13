import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));
const { createUart0 } = await import(join(__dirname, '..', 'src', 'uart0.js'));
const { createI2c } = await import(join(__dirname, '..', 'src', 'i2c.js'));

const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const UART_WINDOW = 0x1000;
const PROG = join(__dirname, '..', 'public', 'programs', 'i2c.elf');

const I2C_BASE = 0x3f804000;

const ucMod = await MUnicorn();
const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
uc.mem_map(0x3f201000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(I2C_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
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

const i2c = createI2c(uc, ucMod, I2C_BASE);
const { state, syncOut, syncIn } = i2c;

function slice() {
  const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || uc.entry;
  syncOut(uc);
  uart0.syncOut(uc);
  uc.emu_start(pc, 0, 0, SLICE_INSNS);
  syncIn(uc);
  uart0.syncIn(uc);
  chars += drain();
}

for (let i = 0; i < 30000 && !state.done; i++) slice();

const want = {
  banner: chars.includes('i2c: BCM2835 BSC master @ 0x3F804000'),
  whoami: chars.includes('i2c: WHO_AM_I = 0x68'),
  temp: chars.includes('i2c: temp = 26 C'),
  counter: chars.includes('i2c: counter = 3 (reads 1, 2, 3)'),
  passed: chars.includes('i2c: all checks passed'),
  parked: chars.includes('i2c: parked'),
  hostDone: state.done,
  slaveCounter: state.counter === 3, // 3 read transfers of REG_COUNTER
  lastRegSelected: state.reg === 0x20, // slave latched the counter register
};

console.log('i2c-probe:');
for (const [k, v] of Object.entries(want)) {
  console.log('  ' + (v ? 'ok ' : 'FAIL') + ' ' + k + (v ? '' : ' (want)'));
}
const pass = Object.values(want).every(Boolean);
console.log(pass ? 'i2c-probe: PASS' : 'i2c-probe: FAIL');
if (!pass) {
  console.log('--- guest output ---');
  console.log(chars.replace(/\r/g, ''));
}
uc.close();
process.exit(pass ? 0 : 1);
