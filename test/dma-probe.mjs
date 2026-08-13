import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));
const { createUart0 } = await import(join(__dirname, '..', 'src', 'uart0.js'));
const { dmaRunChain } = await import(join(__dirname, '..', 'src', 'dma.js'));

const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const UART_WINDOW = 0x1000;
const PROG = join(__dirname, '..', 'public', 'programs', 'dma.elf');

const DMA_BASE = 0x3f007000;
const DMA_CS = DMA_BASE + 0x00;
const DMA_CONBLK = DMA_BASE + 0x04;
const DMA_ENABLE = 0x3f00e050;
const DMA_DONE = 0x3f00e054;

const IC_BASE = 0x3f00b200;
const IC_PENDING1 = IC_BASE + 0x04;
const IC_ENABLE_IRQS1 = IC_BASE + 0x10;
const IC_IRQ_RET = IC_BASE + 0x2c;
const IRQ_DMA0 = 1 << 16;

const SRC = 0x285000;
const DST = 0x286000;
const DST2 = 0x287000;
const DST3 = 0x288000;

const ucMod = await MUnicorn();
const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
uc.mem_map(0x3f201000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(DMA_BASE, 0x2000, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(0x3f00e000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(0x3f00b000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
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
let delivered = 0;
let dmaEnd = false;
let dmaInt = false;
let intSeen = false;
let dmaEnable = 0;
let dmaLastCS = 0;
let dmaDone = false;
let icEnabled1 = 0;
let irqElr = 0;
let irqInFlight = false;
let irqVector = 0;
let irqResume = 0;

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
function syncDmaOut() {
  let cs = 0;
  if (dmaEnd) cs |= 2;
  if (dmaInt) cs |= 4;
  writeU32(DMA_CS, cs);
  dmaLastCS = cs;
  writeU32(DMA_ENABLE, dmaEnable);
}
function syncDmaIn() {
  const cs = readU32(DMA_CS);
  const conblk = readU32(DMA_CONBLK);
  dmaEnable = readU32(DMA_ENABLE);
  if (readU32(DMA_DONE) !== 0) dmaDone = true;
  if (cs & (1 << 31)) {
    dmaEnd = false;
    dmaInt = false;
  } else {
    if ((cs & 1) && !(dmaLastCS & 1) && conblk !== 0 && (dmaEnable & 1)) {
      const r = dmaRunChain(uc, conblk);
      dmaEnd = true;
      if (r.int) {
        dmaInt = true;
        intSeen = true;
      }
    }
    if (dmaInt && (dmaLastCS & 4) !== 0 && (cs & 4) === 0) dmaInt = false; // guest cleared INT
  }
}
function syncIcOut() {
  let p1 = 0;
  if (dmaInt && (dmaEnable & 1)) p1 |= IRQ_DMA0;
  writeU32(IC_PENDING1, p1);
}
function syncIcIn() {
  const en = readU32(IC_ENABLE_IRQS1);
  if (en) {
    icEnabled1 |= en;
    writeU32(IC_ENABLE_IRQS1, 0);
  }
}
function syncIrqRet() {
  const r = readU32(IC_IRQ_RET);
  if (r !== 0) {
    writeU32(IC_IRQ_RET, 0);
    irqResume = irqElr;
  }
}
function irqDeliver() {
  let pending = readU32(IC_PENDING1) & icEnabled1;
  // The PENDING1 window is refreshed before each slice, so it can still hold
  // the DMA0 bit after the guest has cleared CS.INT. Re-derive the line from
  // host state: once INT is cleared, a stale window must not re-deliver.
  if (!(dmaInt && (dmaEnable & 1))) pending &= ~IRQ_DMA0;
  if (!pending || irqInFlight) return;
  const pcNow = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC));
  irqElr = pcNow || elf.entry;
  irqInFlight = true;
  const vbar = Number(uc.reg_read_i32(ucMod.ARM64_REG_VBAR_EL1)) || 0x100000;
  irqVector = vbar + 0x280;
  delivered++;
}
function slice() {
  const pc = irqResume || irqVector || Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || uc.entry;
  if (irqResume) irqInFlight = false;
  irqResume = 0;
  irqVector = 0;
  syncDmaOut();
  syncIcOut();
  uart0.syncOut(uc);
  uc.emu_start(pc, 0, 0, SLICE_INSNS);
  syncDmaIn();
  syncIcIn();
  syncIrqRet();
  uart0.syncIn(uc);
  chars += drain();
  irqDeliver();
}

uc.hook_add(ucMod.HOOK_CODE, (u, a) => {
});
for (let i = 0; i < 30000 && !dmaDone; i++) slice();

const src = new Uint8Array(64);
const dst = new Uint8Array(uc.mem_read(DST, 64));
const dst2 = new Uint8Array(uc.mem_read(DST2, 32));
const dst3 = new Uint8Array(uc.mem_read(DST3, 16));
for (let i = 0; i < 64; i++) src[i] = 0x5a + i;

const want = {
  banner: chars.includes('dma: BCM2835 DMA controller @ 0x3F007000'),
  armed: chars.includes('dma: channel 0 enabled, IRQ 16 armed'),
  done: chars.includes('dma: chain done (END set)'),
  irq: chars.includes('[dma irq] completed'),
  fullCopy: chars.includes('dma: full copy OK (64 bytes)'),
  relayCopy: chars.includes('dma: relay copy OK (32 bytes)'),
  fill: chars.includes('dma: fill OK (SRC_IGNORE, 16 bytes)'),
  passed: chars.includes('dma: all checks passed'),
  parked: chars.includes('dma: parked'),
  delivered: delivered >= 1,
  dstMatches: dst.every((b, i) => b === src[i]),
  dst2Matches: dst2.every((b, i) => b === src[i]),
  dst3Filled: dst3.every((b) => b === 0x77),
  hostEnd: dmaEnd,
  intSeen: intSeen,
};

console.log('dma-probe:');
for (const [k, v] of Object.entries(want)) {
  console.log('  ' + (v ? 'ok ' : 'FAIL') + ' ' + k + (v ? '' : ' (want)'));
}
const pass = Object.values(want).every(Boolean);
console.log(pass ? 'dma-probe: PASS (deliveries=' + delivered + ')' : 'dma-probe: FAIL');
console.log('--- guest output ---');
console.log(chars.replace(/\r/g, ''));
uc.close();
process.exit(pass ? 0 : 1);
