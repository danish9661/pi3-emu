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
const MAX_ROUNDS = 2000;
const CORE_COUNT = 4;
const PROG = join(__dirname, '..', 'public', 'programs', 'smp.elf');

const SMP_BASE = 0x3f202000;
const SMP_START = SMP_BASE + 0x00;
const SMP_GO = SMP_BASE + 0x10;
const SMP_COUNTER = SMP_BASE + 0x14;
const SMP_LOCK = SMP_BASE + 0x18;
const SMP_MSG = SMP_BASE + 0x1c;
const SMP_PARK = SMP_BASE + 0x34;
const SMP_CPUID = SMP_BASE + 0x38;

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

  const cores = [];
  const entries = [elf.entry, 0, 0, 0];
  const state = { go: 0, counter: 0, lock: 0, park: 0, msg: [0, 0, 0, 0], start: [0, 0, 0] };
  for (let i = 0; i < CORE_COUNT; i++) {
    const c = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
    c.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
    c.mem_map(uart, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
    c.mem_map(SMP_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
    c.devUart = { base: uart };
    loadElf(c, elf);
    cores.push(c);
  }

  const syncOut = (c, i) => {
    writeU32(c, SMP_CPUID, i);
    writeU32(c, SMP_GO, state.go);
    writeU32(c, SMP_COUNTER, state.counter);
    writeU32(c, SMP_LOCK, state.lock);
    writeU32(c, SMP_PARK, state.park);
    for (let k = 0; k < CORE_COUNT; k++) writeU32(c, SMP_MSG + k * 4, state.msg[k]);
  };
  const syncIn = (c, i) => {
    if (i === 0) {
      for (let k = 0; k < 3; k++) {
        const v = readU32(c, SMP_START + (k + 1) * 4);
        if (v !== 0 && state.start[k] === 0) state.start[k] = v;
      }
      if (readU32(c, SMP_GO) !== 0) state.go = 1;
    }
    const ctr = readU32(c, SMP_COUNTER);
    if (ctr !== state.counter) state.counter = ctr;
    const lk = readU32(c, SMP_LOCK);
    if (lk !== state.lock) state.lock = lk;
    if (state.msg[i] === 0) state.msg[i] = readU32(c, SMP_MSG + i * 4);
    state.park |= readU32(c, SMP_PARK);
  };
  // Each core prints through the PL011 DR: the TX write hook pushes chars
  // straight into the board console (the slots are gone).
  for (const c of cores) {
    createUart0(c, ucMod, uart, (b) => board.pi_cons_push(b));
  }
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
  let chars = '';
  let endRounds = -1;
  const started = [true, false, false, false];
  const allParked = (1 << CORE_COUNT) - 1;
  outer: for (let r = 0; r < MAX_ROUNDS; r++) {
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
      const pc = Number(c.reg_read_i32(ucMod.ARM64_REG_PC)) || entries[i];
      c.emu_start(pc, 0, 0, SLICE_INSNS);
      syncIn(c, i);
    }
    chars += drain();
    if (state.park === allParked) {
      chars += drain();
      endRounds = r;
      break outer;
    }
  }
  const elapsed = Date.now() - t0;

  console.log('console output:', JSON.stringify(chars));
  const wantList = [
    'core 0: sum 1..25 = 325',
    'core 1: sum 26..50 = 950',
    'core 2: sum 51..75 = 1575',
    'core 3: sum 76..100 = 2200',
    'mailbox: 325 950 1575 2200',
    'all cores joined: counter = 4',
  ];
  for (const want of wantList) {
    console.log('contains:', want, '->', chars.includes(want));
  }
  console.log('all cores parked:', state.park === allParked, '| rounds:', endRounds, '| time:', elapsed, 'ms');
  cores.forEach((c) => c.close());
}

main().then(() => process.exit(0)).catch((e) => {
  console.error('FATAL', String(e && e.message || e).slice(0, 300));
  process.exit(1);
});