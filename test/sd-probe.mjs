import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));
const { createUart0 } = await import(join(__dirname, '..', 'src', 'uart0.js'));
const { createSdhci } = await import(join(__dirname, '..', 'src', 'sdhci.js'));

const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const UART_WINDOW = 0x1000;
const PROG = join(__dirname, '..', 'public', 'programs', 'sd.elf');

const SD_BASE = 0x3f300000;

const ucMod = await MUnicorn();
const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
uc.mem_map(0x3f201000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(SD_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
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

const sd = createSdhci(uc, ucMod, SD_BASE);
const { state, syncOut, syncIn } = sd;

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

const seq = state.commands.map(([i]) => i).join(',');
const sectors = state.commands.filter(([i]) => i === 17).map(([, a]) => a);
const want = {
  banner: chars.includes('sd: SDHCI (EMMC) @ 0x3F300000, FAT12 card'),
  cmd8: chars.includes('sd: CMD8 echo OK (v2, 3.3V)'),
  acmd41: chars.includes('sd: ACMD41 ready (high capacity)'),
  rca: chars.includes('sd: RCA = 0x1234'),
  selected: chars.includes('sd: card selected (R1 ready)'),
  bootSector: chars.includes('sd: FAT12 boot sector OK (512B, 2 FATs)'),
  found: chars.includes('sd: HELLO.TXT found in root directory'),
  payload: chars.includes('sd: "hello from the SD card"'),
  payloadMatch: chars.includes('sd: payload matches'),
  passed: chars.includes('sd: all checks passed'),
  parked: chars.includes('sd: parked'),
  hostDone: state.done,
  // The exact SD init sequence then three single-block reads: sector 0
  // (boot), 3 (root dir), 4 (data).
  initSeq: seq === '0,8,55,41,2,3,7,17,17,17',
  sectors: sectors.join(',') === '0,3,4',
  resp0: state.resp[0] === 0x900,
};

console.log('sd-probe:');
for (const [k, v] of Object.entries(want)) {
  console.log('  ' + (v ? 'ok ' : 'FAIL') + ' ' + k + (v ? '' : ' (want)'));
}
const pass = Object.values(want).every(Boolean);
console.log(pass ? 'sd-probe: PASS' : 'sd-probe: FAIL');
if (!pass) {
  console.log('--- guest output ---');
  console.log(chars.replace(/\r/g, ''));
}
uc.close();
process.exit(pass ? 0 : 1);
