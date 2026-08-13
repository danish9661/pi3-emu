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
const MAX_SLICES = 60000;
const PROG = join(__dirname, '..', 'public', 'programs', 'fb.elf');

const TMR_BASE = 0x3f003000;
const TMR_CLO = TMR_BASE + 4;

const MBOX_WINDOW = 0x3f00b000;
const MBOX_BASE = 0x3f00b880;
const MBOX_STATUS = MBOX_BASE + 0x04;
const MBOX_MAIL1_WRITE = MBOX_BASE + 0x14;
const MBOX_MAIL1_STATUS = MBOX_BASE + 0x18;
const CHANNEL = 8;

const FB_ADDR = 0x200000;
const FB_W = 160;
const FB_H = 120;
const FB_PITCH = FB_W * 4;
const FB_BYTES = FB_PITCH * FB_H;

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}
function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}
function readU8(uc, addr) {
  return uc.mem_read(addr, 1)[0];
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

  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
  uc.mem_map(uart, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(TMR_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(MBOX_WINDOW, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.devUart = { base: uart };
  loadElf(uc, elf);

  const wall0 = Date.now();
  let fbW = 0;
  let fbH = 0;
  let fbDepth = 0;
  let fbPitch = 0;
  let chars = '';

  const syncTmr = () => writeU32(uc, TMR_CLO, ((Date.now() - wall0) * 1000) & 0xffffffff);

  // Host-side mailbox mirror (same fb tags as src/main.js).
  let mbxAddr = 0;
  let mbxLast = 0;
  let mbxPending = false;
  const fbTag = (addr, off, id) => {
    const v = addr + off + 12;
    switch (id) {
      case 0x00048003:
      case 0x00048004:
        fbW = readU32(uc, v);
        fbH = readU32(uc, v + 4);
        writeU32(uc, addr + off + 8, 0x80000000);
        return true;
      case 0x00048005:
        fbDepth = readU32(uc, v);
        writeU32(uc, addr + off + 8, 0x80000000);
        return true;
      case 0x00048006:
        writeU32(uc, addr + off + 8, 0x80000000);
        return true;
      case 0x00040001:
        fbPitch = FB_PITCH;
        writeU32(uc, addr + off + 8, 0x80000000);
        writeU32(uc, v, FB_ADDR);
        writeU32(uc, v + 4, fbPitch);
        return true;
      case 0x00040008:
        writeU32(uc, addr + off + 8, 0x80000000);
        writeU32(uc, v, fbPitch);
        return true;
      default:
        return false;
    }
  };
  const mboxProcess = (addr) => {
    const size = Math.min(readU32(uc, addr) & 0xffff, 1024);
    let off = 8;
    while (off + 8 <= size) {
      const id = readU32(uc, addr + off);
      if (id === 0) break;
      const tsize = readU32(uc, addr + off + 4);
      if (!fbTag(addr, off, id)) writeU32(uc, addr + off + 8, 0x80000001);
      off += 12 + tsize + ((4 - (tsize % 4)) % 4);
    }
    writeU32(uc, addr + 4, 0x80000000);
    mbxPending = true;
  };
  const syncMbxOut = () => {
    writeU32(uc, MBOX_MAIL1_STATUS, 0);
    if (mbxPending) {
      writeU32(uc, MBOX_STATUS, 0);
      writeU32(uc, MBOX_BASE + 0x00, mbxAddr);
    } else {
      writeU32(uc, MBOX_STATUS, 0x80000000);
      writeU32(uc, MBOX_BASE + 0x00, 0);
    }
  };
  const syncMbxIn = () => {
    const w = readU32(uc, MBOX_MAIL1_WRITE);
    if (w !== mbxLast) {
      mbxLast = w;
      mbxAddr = w;
      if ((w & 0xf) === CHANNEL) mboxProcess(w & ~0xf);
    }
  };

  const uart0 = createUart0(uc, ucMod, uc.devUart.base, (b) => board.pi_cons_push(b));
  const drain = () => {
    let out = '';
    for (;;) {
      const ch = Number(board.pi_cons_poll());
      if (ch === -1 || ch === 0xffffffff) break;
      out += String.fromCharCode(ch);
    }
    return out;
  };
  const slice = () => {
    syncTmr();
    syncMbxOut();
    const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || elf.entry;
    uart0.syncOut(uc);
    uc.emu_start(pc, 0, 0, SLICE_INSNS);
    syncMbxIn();
    uart0.syncIn(uc);
    return drain();
  };
  const pixel = (x, y) => {
    const b = uc.mem_read(FB_ADDR + y * FB_PITCH + x * 4, 4);
    return (b[0] << 16) | (b[1] << 8) | b[2]; // RGB
  };
  const anyYellowInBox = (cx, cy, half) => {
    for (let y = cy - half; y <= cy + half; y++) {
      for (let x = cx - half; x <= cx + half; x++) {
        if (x < 0 || y < 0 || x >= FB_W || y >= FB_H) continue;
        if (pixel(x, y) === 0xffff00) return true;
      }
    }
    return false;
  };

  // Phase 1: run until the guest allocates the fb, prints the pattern.
  const t0 = Date.now();
  let sawPattern = false;
  for (let i = 0; i < MAX_SLICES && !sawPattern; i++) {
    chars += slice();
    if (chars.includes('pattern drawn')) sawPattern = true;
  }
  const fbLine = chars.split('\n').find((l) => l.startsWith('fb: ')) || '';
  console.log('fb line:', JSON.stringify(fbLine));
  console.log('pattern drawn:', sawPattern);

  // Phase 2: verify deterministic pattern pixels (red bg, blue border, ball).
  const r00 = pixel(0, 0);
  const rEdge = pixel(FB_W - 1, FB_H - 1);
  const bBorder = pixel(2, 2);
  const ballAtCenter = anyYellowInBox(FB_W / 2, FB_H / 2, 8);

  // Phase 3: liveness — the ball must have moved after ~150 ms of wall time.
  const snapA = uc.mem_read(FB_ADDR, FB_BYTES);
  const twall = Date.now();
  while (Date.now() - twall < 150) chars += slice();
  const snapB = uc.mem_read(FB_ADDR, FB_BYTES);
  let moved = 0;
  for (let i = 0; i < FB_BYTES; i += 4) if (snapA[i] !== snapB[i]) moved++;
  console.log('ball moved pixels:', moved);

  const checks = [
    ['fb line mentions fb:', fbLine.includes('fb: ')],
    ['fb line has pitch 640:', fbLine.includes(' pitch 640 ')],
    ['fb line has 160x120:', fbLine.includes('160x120')],
    ['pattern drawn:', chars.includes('pattern drawn')],
    ['pixel(0,0) red 0xff0000:', r00 === 0xff0000],
    ['pixel(159,119) red:', rEdge === 0xff0000],
    ['pixel(2,2) blue 0x0000ff:', bBorder === 0x0000ff],
    ['yellow ball near center:', ballAtCenter],
    ['ball moved after 150ms:', moved > 0],
  ];
  for (const [name, ok] of checks) console.log(ok ? 'PASS' : 'FAIL', '-', name);
  console.log('session time:', Date.now() - t0, 'ms');
  uc.close();
  process.exit(checks.every(([, ok]) => ok) ? 0 : 1);
}

main().catch((e) => {
  console.error('FATAL', String(e && e.message || e).slice(0, 300));
  process.exit(1);
});