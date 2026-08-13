import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));
const { createSpi } = await import(join(__dirname, '..', 'src', 'spi.js'));

const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const UART_WINDOW = 0x1000;
const TX_SLOT_STRIDE = 4;
const TX_SLOTS = 32;
const PROG = join(__dirname, '..', 'public', 'programs', 'spi.elf');

const SPI_BASE = 0x3f204000;

const ucMod = await MUnicorn();
const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
uc.mem_map(0x3f201000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(SPI_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
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

const spi = createSpi(uc, ucMod, SPI_BASE);
const { state, syncOut, syncIn } = spi;

function slice() {
  const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || uc.entry;
  syncOut(uc);
  uc.emu_start(pc, 0, 0, SLICE_INSNS);
  syncIn(uc);
  chars += pump();
}

for (let i = 0; i < 30000 && !state.done; i++) slice();

const want = {
  banner: chars.includes('spi: SPI0 master @ 0x3F204000'),
  jedec: chars.includes('spi: JEDEC ID = 0xEF 0x40 0x18 (Winbond W25Q128)'),
  identical: chars.includes('spi: both transactions identical (CLEAR resets OK)'),
  passed: chars.includes('spi: all checks passed'),
  parked: chars.includes('spi: parked'),
  hostDone: state.done,
  // Two transactions happened and each CLEAR reset the FIFOs/session.
  txDrained: state.tx.length === 0,
  rxDrained: state.rx.length === 0,
};

console.log('spi-probe:');
for (const [k, v] of Object.entries(want)) {
  console.log('  ' + (v ? 'ok ' : 'FAIL') + ' ' + k + (v ? '' : ' (want)'));
}
const pass = Object.values(want).every(Boolean);
console.log(pass ? 'spi-probe: PASS' : 'spi-probe: FAIL');
if (!pass) {
  console.log('--- guest output ---');
  console.log(chars.replace(/\r/g, ''));
}
uc.close();
process.exit(pass ? 0 : 1);
