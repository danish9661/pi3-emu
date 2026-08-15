import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';
import { createLocalInt } from '../src/localint.js';
import { createIc } from '../src/ic.js';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));

const PROG = join(__dirname, '..', 'public', 'programs', 'lirq.elf');
const RAM_SIZE = 0x400000;
const CNTP_TVAL = 0x1000;

const TMR_BASE = 0x3f003000;
const TMR_CS = TMR_BASE + 0x00;
const TMR_CLO = TMR_BASE + 0x04;
const TMR_CMP = TMR_BASE + 0x0c;
const IC_BASE = 0x3f00b200;
const LOCAL_BASE = 0x40000000;
const LOCAL_IRQ_SRC = LOCAL_BASE + 0x60;

function readU64(uc, addr) {
  const b = uc.mem_read(addr, 8);
  return BigInt(b[0]) + BigInt(b[1]) * 2n ** 8n + BigInt(b[2]) * 2n ** 16n + BigInt(b[3]) * 2n ** 24n +
         BigInt(b[4]) * 2n ** 32n + BigInt(b[5]) * 2n ** 40n + BigInt(b[6]) * 2n ** 48n + BigInt(b[7]) * 2n ** 56n;
}

function findSymbol(buf, name) {
  const u16 = (o) => buf[o] | (buf[o + 1] << 8);
  const u32 = (o) => buf[o] | (buf[o + 1] << 8) | (buf[o + 2] << 16) | (buf[o + 3] << 24);
  const u64 = (o) => u32(o) + u32(o + 4) * 2 ** 32;
  const shoff = u64(40), shentsize = u16(58), shnum = u16(60);
  for (let i = 0; i < shnum; i++) {
    const o = shoff + i * shentsize;
    if (u32(o + 4) !== 2) continue;
    const link = u32(o + 40), off = u64(o + 24), size = u64(o + 32), ents = u64(o + 56);
    const strOff = u64(shoff + link * shentsize + 24);
    for (let s = 0; s < size / ents; s++) {
      const so = off + s * ents;
      const stName = u32(so), stValue = u64(so + 8);
      let n = strOff + stName, nm = '';
      while (buf[n]) nm += String.fromCharCode(buf[n++]);
      if (nm === name) return stValue;
    }
  }
  throw new Error('symbol not found: ' + name);
}

let ucMod;
async function main() {
  ucMod = await MUnicorn();
  const elfBuf = new Uint8Array(readFileSync(PROG));
  const elf = parseElf(elfBuf);
  const scratch = findSymbol(elfBuf, 'SCRATCH');
  let pass = 0, fail = 0;
  const check = (label, cond, detail = '') => {
    if (cond) { pass++; console.log(`  PASS ${label}${detail ? ' (' + detail + ')' : ''}`); }
    else { fail++; console.log(`  FAIL ${label}${detail ? ' (' + detail + ')' : ''}`); }
  };

  // ---- host device layer (mirrors main.js slice sync, no DOM) ----
  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
  uc.mem_map(TMR_BASE, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(0x3f00b000, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(LOCAL_BASE, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(0x3f201000, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE); // PL011 for puts
  uc.entry = loadElf(uc, elf);

  const writeU32 = (uc, addr, v) =>
    uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
  const readU32 = (uc, addr) => {
    const b = uc.mem_read(addr, 4);
    return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
  };

  // synthetic clock: the host jumps CLO, like the browser's wall clock but
  // deterministic
  let clo = 0;
  let tmrPending = 0;
  let tmrLastCS = 0;
  let tmrCompares = [0, 0, 0, 0];
  let tmrCrossed = [false, false, false, false];

  function syncTimerOut() {
    writeU32(uc, TMR_CLO, clo);
    writeU32(uc, TMR_CLO + 4, 0);
    for (let i = 0; i < 4; i++) {
      const c = tmrCompares[i];
      if (!tmrCrossed[i] && c !== 0 && ((clo - c) & 0x80000000) === 0) {
        tmrCrossed[i] = true;
        tmrPending |= 1 << i;
      }
    }
    writeU32(uc, TMR_CS, tmrPending);
    tmrLastCS = tmrPending;
  }

  function syncTimerIn() {
    for (let i = 0; i < 4; i++) {
      const c = readU32(uc, TMR_CMP + i * 4);
      if (c !== tmrCompares[i]) tmrCrossed[i] = false;
      tmrCompares[i] = c;
    }
    const cs = readU32(uc, TMR_CS);
    if (cs !== tmrLastCS) tmrPending &= cs & 0xf;
  }

  // The real 3-bank legacy IC: Phase B enables C3 (bank-1 bit 3).
  const ic = createIc(uc, ucMod, IC_BASE, () => ({
    timer: tmrPending & 0xf,
    dma0: false,
    pl011: false,
    sdhci: false,
    gpio0: false,
    gpio1: false,
    aux: false,
  }));
  const gpuLine = () => ic.line();

  const local = createLocalInt(uc, ucMod, LOCAL_BASE, () => ({
    cntps: uc.arm64_debug(13) ? 1 : 0,
    cntpns: uc.arm64_debug(3) ? 1 : 0,
    cnthp: uc.arm64_debug(12) ? 1 : 0,
    cntv: uc.arm64_debug(11) ? 1 : 0,
    gpu: gpuLine(),
    pmu: 0,
    axi: 0,
    ltimer: 0,
    mailbox: [0, 0, 0, 0],
  }));
  const setLine = (level) => uc.arm64_set_irq(level);

  // real-time ack: TMR_CS writes re-derive the line immediately
  uc.hook_add(
    ucMod.HOOK_MEM_WRITE,
    (u, access, addr, size, value) => {
      if (Number(addr) !== TMR_CS) return;
      tmrPending &= Number(value) & 0xf;
      local.syncIrq(uc, setLine);
    },
    null,
    TMR_CS,
    TMR_CS + 3
  );

  function runSlice(count = 512) {
    syncTimerOut();
    ic.syncOut(uc);
    local.syncOut(uc);
    local.syncIrq(uc, setLine);
    let pc = Number(uc.reg_read_i64(ucMod.ARM64_REG_PC));
    if (!pc) pc = uc.entry;
    uc.emu_start(pc, 0, 0, count);
    syncTimerIn();
    ic.syncIn(uc);
    local.syncIn(uc);
  }

  // ---------------- Phase A: CNTPNS (local source bit 1) ----------------
  console.log('Phase A: arch timer -> local block source bit 1 (CNTPNS)');
  runSlice(2048); // guest arms CNTP and spins
  const pcSpin = Number(uc.reg_read_i64(ucMod.ARM64_REG_PC));
  check('guest spinning with timer armed', pcSpin > 0x100000, 'pc=0x' + pcSpin.toString(16));

  uc.arm64_timer_tick(CNTP_TVAL + 0x10000); // count past cval
  runSlice(4096); // delivery inside this slice: handler runs + erets
  check('handler ran (flag A=1)', readU64(uc, scratch + 32) === 1n);
  check('local source reg shows CNTPNS bit 1', readU64(uc, scratch + 16) === 2n,
    'src=0x' + readU64(uc, scratch + 16).toString(16));
  check('ELR_EL1 = interrupted PC', readU64(uc, scratch) === BigInt(pcSpin),
    'elr=0x' + readU64(uc, scratch).toString(16));
  check('SPSR_EL1 I clear at entry', (readU64(uc, scratch + 8) & 0x80n) === 0n);
  check('CNTPCT advanced past cval', readU64(uc, scratch + 24) >= BigInt(CNTP_TVAL + 0x10000));
  runSlice(512);
  check('timer disabled in handler: no re-entry', readU64(uc, scratch + 32) === 1n);

  // ---------------- Phase B: GPU (local source bit 8) ----------------
  console.log('Phase B: system timer C3 -> IC -> GPU line -> local bit 8');
  runSlice(2048); // guest enables the line in the IC and programs the compare
  const cmp = tmrCompares[3];
  check('guest programmed the compare', cmp !== 0, 'cmp=0x' + cmp.toString(16));
  clo = cmp + 0x10000; // jump the clock past the compare
  runSlice(4096); // match fires -> GPU line -> delivery
  check('handler ran (flag B=2)', readU64(uc, scratch + 72) === 2n,
    'flag=0x' + readU64(uc, scratch + 72).toString(16));
  check('local source reg shows GPU bit 8', readU64(uc, scratch + 56) === 0x100n,
    'src=0x' + readU64(uc, scratch + 56).toString(16));
  check('TMR_CS showed the C3 match pending', (readU64(uc, scratch + 64) & 8n) !== 0n,
    'cs=0x' + readU64(uc, scratch + 64).toString(16));
  runSlice(512);
  check('ack cleared C3: no re-entry', readU64(uc, scratch + 72) === 2n);
  check('GPU line de-asserted', Number(uc.arm64_debug(2)) === 0);
  check('ext IRQ line clean', (Number(uc.arm64_debug(0)) & 2) === 0);

  console.log(`\nlirq probe: ${pass} passed, ${fail} failed`);
  uc.close();
  process.exit(fail === 0 ? 0 : 1);
}

main().catch((e) => { console.error(e); process.exit(1); });
