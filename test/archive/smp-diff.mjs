import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

// SMP differential: quad-unicorn oracle (partitioned RAM + shared host
// mailbox, mirroring test/smp-probe.mjs wiring but asserting full state)
// vs pi-cpu SmpRunner (PI3_SMP=1). Compares console lines, park mask,
// counter, per-core message boxes, and fault presence.
const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');
const require = createRequire(import.meta.url);
const MUnicorn = require(join(ROOT, 'public', 'unicorn.js'));
const ucMod = await MUnicorn();
const { parseElf, loadElf } = await import(join(ROOT, 'packages', 'pi3-emu', 'src', 'elf.js'));

const CORE_COUNT = 4;
const SLICE_INSNS = 512;
const RAM_SIZE = 0x400000;
const UART_WINDOW = 0x1000;
const UART0 = 0x3f201000;
const SMP_BASE = 0x3f202000;

const budget = Number(process.argv[2] || '4000000');
const slice = Number(process.argv[3] || '512');

function writeU32(uc, addr, v) {
  uc.mem_write(addr, Buffer.from([(v >>> 0) & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]));
}
function readU32(uc, addr) {
  const b = Buffer.from(uc.mem_read(addr, 4));
  return (b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24)) >>> 0;
}

// ---- quad-unicorn oracle ----
const PROG = join(ROOT, 'public', 'programs', 'smp.elf');
const elf = parseElf(new Uint8Array(readFileSync(PROG)));
const cores = [];
const entries = [elf.entry, 0, 0, 0];
const state = { go: 0, counter: 0, lock: 0, park: 0, msg: [0, 0, 0, 0], start: [0, 0, 0] };
let uchars = '';
for (let i = 0; i < CORE_COUNT; i++) {
  const c = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  c.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
  c.mem_map(UART0, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  c.mem_map(SMP_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  c.hook_add(ucMod.HOOK_MEM_WRITE, (u, access, address, size, value) => {
    const b = Number(value) & 0xff;
    if (b !== 0) uchars += String.fromCharCode(b);
  }, null, UART0, UART0 + 3);
  loadElf(c, elf);
  cores.push(c);
}
const syncOut = (c, i) => {
  writeU32(c, SMP_BASE + 0x38, i);
  writeU32(c, SMP_BASE + 0x30, i);
  writeU32(c, SMP_BASE + 0x10, state.go);
  writeU32(c, SMP_BASE + 0x14, state.counter);
  writeU32(c, SMP_BASE + 0x18, state.lock);
  writeU32(c, SMP_BASE + 0x34, state.park);
  for (let k = 0; k < CORE_COUNT; k++) writeU32(c, SMP_BASE + 0x1c + k * 4, state.msg[k]);
};
const syncIn = (c, i) => {
  if (i === 0) {
    for (let k = 0; k < 3; k++) {
      const v = readU32(c, SMP_BASE + (k + 1) * 4);
      if (v !== 0 && state.start[k] === 0) state.start[k] = v;
    }
    if (readU32(c, SMP_BASE + 0x10) !== 0) state.go = 1;
  }
  const ctr = readU32(c, SMP_BASE + 0x14);
  if (ctr !== state.counter) state.counter = ctr;
  const lk = readU32(c, SMP_BASE + 0x18);
  if (lk !== state.lock) state.lock = lk;
  if (state.msg[i] === 0) state.msg[i] = readU32(c, SMP_BASE + 0x1c + i * 4);
  state.park |= readU32(c, SMP_BASE + 0x34);
};
const started = [true, false, false, false];
const allParked = (1 << CORE_COUNT) - 1;
let ufault = null;
let utotal = 0;
outer: for (let r = 0; r < 2000; r++) {
  for (let i = 0; i < CORE_COUNT; i++) {
    if (!started[i]) {
      const e = state.start[i - 1];
      if (e === 0) continue;
      started[i] = true;
      entries[i] = e;
    }
    if (state.park & (1 << i)) continue;
    const c = cores[i];
    syncOut(c, i);
    let pc = 0;
    try { pc = Number(c.reg_read_i32(ucMod.ARM64_REG_PC)); } catch {}
    try {
      c.emu_start(pc || entries[i], 0, 0, slice);
      utotal += slice;
    } catch (e) { ufault = String(e).split('\n')[0].slice(0, 60); break outer; }
    syncIn(c, i);
  }
  if (state.park === allParked || utotal >= budget) break outer;
}

// ---- quad-pi-cpu ----
const out = execFileSync(join(ROOT, 'target', 'release', 'examples', 'run'),
  [PROG, String(budget), String(slice), '0'], { maxBuffer: 64 * 1024 * 1024, env: { ...process.env, PI3_SMP: '1' } }).toString();
const L = Object.fromEntries(out.trim().split('\n').map((l) => {
  const j = l.indexOf('\t');
  return [l.slice(0, j), l.slice(j + 1)];
}));
const pcon = JSON.parse(`"${L.console}"`);
const psmp = L.smp.split(' ');

let fails = 0;
const check = (name, a, b) => {
  const ok = JSON.stringify(a) === JSON.stringify(b);
  console.log(ok ? 'ok' : 'FAIL', name, ok ? '' : `\n  unicorn: ${JSON.stringify(a).slice(0, 200)}\n  pi-cpu:  ${JSON.stringify(b).slice(0, 200)}`);
  if (!ok) fails++;
};
check('console', uchars, pcon);
check('park', state.park, Number(psmp[0]));
check('counter', state.counter, Number(psmp[1]));
check('msg', state.msg, psmp.slice(2, 6).map(Number));
check('fault', ufault === null, psmp[6] === 'null');
const wantList = [
  'core 0: sum 1..25 = 325',
  'core 1: sum 26..50 = 950',
  'core 2: sum 51..75 = 1575',
  'core 3: sum 76..100 = 2200',
  'mailbox: 325 950 1575 2200',
  'all cores joined: counter = 4',
];
for (const w of wantList) check(`has ${w}`, true, pcon.includes(w));
if (fails) process.exit(1);
console.log('smp-diff: PASS');
