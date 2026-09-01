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

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}
function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}

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
const { state, syncOut, syncIn, irqActive } = sd;

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

// Phase 2: IRQ line semantics
console.log('sd-probe IRQ model:');
const IRPT_EN = SD_BASE + 0x34;
const IRPT_MASK = SD_BASE + 0x38;
const IRPT_CMD_COMPLETE = 1;
const irqChecks = [];
const irqCheck = (label, cond) => {
  irqChecks.push(cond);
  console.log('  ' + (cond ? 'ok ' : 'FAIL') + ' ' + label);
};
state.irq = IRPT_CMD_COMPLETE;
irqCheck('line low with IRPT_EN=0 (raw set)', irqActive() === false);
state.irq = 0;
writeU32(uc, IRPT_EN, IRPT_CMD_COMPLETE);
writeU32(uc, IRPT_MASK, IRPT_CMD_COMPLETE);
syncIn(uc);
sd.exec(7, 0);
irqCheck('line high after CMD with EN|MASK set', irqActive() === true);
syncOut(uc);
irqCheck('INTERRUPT window shows CMD_COMPLETE', readU32(uc, SD_BASE + 0x30) & IRPT_CMD_COMPLETE);
sd.w1c(IRPT_CMD_COMPLETE);
irqCheck('W1C clears the line', irqActive() === false);
writeU32(uc, IRPT_MASK, 0);
syncIn(uc);
sd.exec(7, 0);
irqCheck('line low when MASK=0 (status still set)', irqActive() === false);
irqCheck('raw status still latched', (state.irq & IRPT_CMD_COMPLETE) !== 0);

uc.close();
process.exit(pass && irqChecks.every(Boolean) ? 0 : 1);
