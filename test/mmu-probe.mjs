import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));
const { mmuWalk, mmuEnable, mmuMirrorWrite } = await import(join(__dirname, '..', 'src', 'mmu.js'));

const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const MMU_CTL = 0x3f00d000;
const UART_WINDOW = 0x1000;
const TX_SLOT_STRIDE = 4;
const TX_SLOTS = 32;
const PROG = join(__dirname, '..', 'public', 'programs', 'mmu.elf');

const DATA_PA = 0x200000;
const ALIAS_VA = 0x80000000;

const ucMod = await MUnicorn();
const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
uc.mem_map(0x3f201000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(MMU_CTL, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
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
let mmuState = null;
let mmuHook = null;
let mmuCtl = 0;
let lastPc = 0;
let faults = 0;
let enabled = false;

function writeU32(addr, v) {
  uc.mem_write(
    addr,
    new Uint8Array([v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff])
  );
}
function readU32(addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}
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
function syncMmuOut() {
  if (mmuState) writeU32(MMU_CTL, (mmuState.enabled ? 1 : 0) | mmuState.root);
}
function syncMmuIn() {
  const v = readU32(MMU_CTL);
  if (v === mmuCtl) return;
  mmuCtl = v;
  if (v & 1) {
    mmuState = mmuEnable(uc, ucMod, v & ~1);
    enabled = true;
    if (mmuHook) uc.hook_del(mmuHook);
    mmuHook = uc.hook_add(ucMod.HOOK_MEM_WRITE, (u, access, addr, size, value) => {
      mmuMirrorWrite(uc, mmuState, Number(addr), Number(size), value);
    });
  } else {
    if (mmuHook) {
      uc.hook_del(mmuHook);
      mmuHook = null;
    }
    mmuState = null;
  }
}
function slice() {
  const pc = lastPc || Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || uc.entry;
  syncMmuOut();
  try {
    uc.emu_start(pc, 0, 0, SLICE_INSNS);
  } catch (e) {
    faults++; // burst: guest outruns the host's between-slice enable, retries
  }
  syncMmuIn();
  chars += pump();
}

// 1. MMU must be off at boot: the alias VA is not mapped yet.
let preOk = false;
try {
  uc.mem_read(ALIAS_VA, 4);
} catch (e) {
  preOk = true;
}

// 2. Run the guest (it builds the tables, enables the MMU, runs the checks).
uc.hook_add(ucMod.HOOK_CODE, (u, a) => {
  lastPc = Number(a);
});
for (let i = 0; i < 30000 && !chars.includes('mmu: parked'); i++) slice();

// 3. Verify the alias stays coherent with the identity map.
const pa0 = readU32(DATA_PA);
const pa4 = readU32(DATA_PA + 4);
const pa8 = readU32(DATA_PA + 8);
const va0 = readU32(ALIAS_VA);
const va4 = readU32(ALIAS_VA + 4);

// 4. A VA with no block mapping must stay unmapped (translation fault).
let badOk = false;
try {
  uc.mem_read(ALIAS_VA + 0x200000, 4);
} catch (e) {
  badOk = true;
}

const want = {
  banner: chars.includes('mmu: host-assisted MMU @ 0x3F00D000'),
  tables: chars.includes('mmu: tables at 0x280000, enabling...'),
  aliasWrite: chars.includes('mmu: alias write -> PA read OK'),
  paWrite: chars.includes('mmu: PA write -> alias read OK'),
  shadowCode: chars.includes('mmu: shadow-code call OK'),
  passed: chars.includes('mmu: all checks passed'),
  parked: chars.includes('mmu: parked'),
  preUnmapped: preOk,
  pa0: pa0 === 0x5a5a,
  pa4: pa4 === 0xbeef,
  pa8: pa8 === 0xdead,
  va0: va0 === 0x5a5a,
  va4: va4 === 0xbeef,
  badUnmapped: badOk,
  enabled: enabled,
};

console.log('mmu-probe:');
for (const [k, v] of Object.entries(want)) {
  console.log('  ' + (v ? 'ok ' : 'FAIL') + ' ' + k + (v ? '' : ' (want)'));
}
const pass = Object.values(want).every(Boolean);
console.log(pass ? 'mmu-probe: PASS (faults=' + faults + ')' : 'mmu-probe: FAIL');
console.log('--- guest output ---');
console.log(chars.replace(/\r/g, ''));
uc.close();
process.exit(pass ? 0 : 1);