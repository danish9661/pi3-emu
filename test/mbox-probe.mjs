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
const MAX_SLICES = 5000;
const PROG = join(__dirname, '..', 'public', 'programs', 'shell.elf');

const MBOX_BASE = 0x3f00b880;
const MBOX_READ = MBOX_BASE;
const MBOX_STATUS = MBOX_BASE + 0x04;
const MBOX_MAIL1_WRITE = MBOX_BASE + 0x14;
const MBOX_MAIL1_STATUS = MBOX_BASE + 0x18;
const MBOX_CHANNEL = 8;

const TMR_CLO = 0x3f003004;

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}
function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}
function u32le(b) {
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}

function leBytes(n, len) {
  const b = [];
  for (let i = 0; i < len; i++) b.push(Number((BigInt(n) >> BigInt(i * 8)) & 0xffn));
  return b;
}
const TAGS = {
  0x00010001: (v, ts) => leBytes(16968947, ts),
  0x00010002: (v, ts) => leBytes(0xa02082, ts),
  0x00010003: (v, ts) => leBytes(0xdeadbeef00000000n, ts),
  0x00010005: (v, ts) => leBytes(0, 4).concat(leBytes(0x400000, 4)),
  0x00010009: (v, ts) => [0xb8, 0x27, 0xeb, 0xde, 0xad, 0xbe],
  0x00030001: (v, ts) => leBytes(v, 4).concat(leBytes(1, 4)),
  0x00030002: (v, ts) => leBytes(700000000, ts),
};

function mboxProcess(uc, addr) {
  const size = Math.min(readU32(uc, addr) & 0xffff, 1024);
  let off = 8;
  while (off + 8 <= size) {
    const id = readU32(uc, addr + off);
    if (id === 0) break;
    const tsize = readU32(uc, addr + off + 4);
    const fn = TAGS[id];
    if (fn) {
      const out = fn(readU32(uc, addr + off + 12), tsize);
      writeU32(uc, addr + off + 8, 0x80000000);
      for (let i = 0; i < tsize; i++) uc.mem_write(addr + off + 12 + i, [out[i] ?? 0]);
    } else {
      writeU32(uc, addr + off + 8, 0x80000001);
    }
    off += 12 + tsize + ((4 - (tsize % 4)) % 4);
  }
  writeU32(uc, addr + 4, 0x80000000);
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
  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
  uc.mem_map(uart, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(TMR_CLO & ~0xfff, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(0x3f00b000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.devUart = { base: uart };
  const uart0 = createUart0(uc, ucMod, uart, (b) => board.pi_cons_push(b));

  const elf = parseElf(new Uint8Array(readFileSync(PROG)));
  loadElf(uc, elf);

  const wall0 = Date.now();
  let mbxLast = 0;
  let mbxPending = false;

  const drain = () => {
    let out = '';
    for (;;) {
      const c = Number(board.pi_cons_poll());
      if (c === -1 || c === 0xffffffff) break;
      out += String.fromCharCode(c);
    }
    return out;
  };

  const syncOut = () => {
    writeU32(uc, TMR_CLO, ((Date.now() - wall0) * 1000) & 0xffffffff);
    writeU32(uc, MBOX_MAIL1_STATUS, 0);
    writeU32(uc, MBOX_STATUS, mbxPending ? 0 : 0x80000000);
    writeU32(uc, MBOX_READ, mbxPending ? mbxAddr() : 0);
  };
  let replyAddr = 0;
  const mbxAddr = () => replyAddr;
  const syncIn = () => {
    const w = readU32(uc, MBOX_MAIL1_WRITE);
    if (w !== mbxLast) {
      mbxLast = w;
      replyAddr = w;
      if ((w & 0xf) === MBOX_CHANNEL) {
        mboxProcess(uc, w & ~0xf);
        mbxPending = true;
      }
    }
  };

  const slice = () => {
    const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || elf.entry;
    syncOut();
    uart0.syncOut(uc);
    uc.emu_start(pc, 0, 0, SLICE_INSNS);
    syncIn();
    uart0.syncIn(uc);
    return drain();
  };

  const runUntilIdle = () => {
    let acc = '';
    let quiet = 0;
    for (let i = 0; i < MAX_SLICES && quiet < 2; i++) {
      const o = slice();
      acc += o;
      quiet = o === 0 ? quiet + 1 : 0;
    }
    return acc;
  };

  const type = (str) => {
    let acc = '';
    for (const ch of str) {
      uart0.push(typeof ch === 'number' ? ch : ch.charCodeAt(0));
      acc += runUntilIdle();
    }
    return acc;
  };

  const t0 = Date.now();
  const chars = (runUntilIdle() + type('mbox\r'));
  const elapsed = Date.now() - t0;

  console.log('console output:', JSON.stringify(chars));
  const wantLines = [
    'board rev:  a02082',
    'serial:     deadbeef00000000',
    'arm memory: 0x0 + 0x400000',
    'arm clock:  700000000 Hz',
  ];
  for (const w of wantLines) console.log('contains:', JSON.stringify(w), '->', chars.includes(w));
  console.log('session time:', elapsed, 'ms');
  uc.close();
}

main().then(() => process.exit(0)).catch((e) => {
  console.error('FATAL', String(e && e.message || e).slice(0, 300));
  process.exit(1);
});