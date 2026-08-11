// Probe v2: unambiguous branch tests. Every path ends in `b .` (0x14000000).
// x1 is set *before* each `b .`, so taken vs fall-through is unambiguous.
import { createRequire } from 'module';
import { join } from 'path';

const require = createRequire(import.meta.url);
const MUnicorn = require(join(import.meta.dirname, '..', 'public', 'unicorn.js'));

const w = (x) => [x & 0xff, (x >> 8) & 0xff, (x >> 16) & 0xff, (x >> 24) & 0xff];
const HALT = 0x14000000;
const B = (off) => 0x14000000 | (off & 0x3ffffff);
const MOVZ = (reg, imm) => 0x52800000 | ((imm & 0xffff) << 5) | reg;
const CMP = (imm) => 0xf1100000 | ((imm & 0xfff) << 10) | 0x1f;
const BEQ = (off) => 0x54000000 | ((off & 0x7ffff) << 5);
const BNE = (off) => 0x54000001 | ((off & 0x7ffff) << 5);
const CBNZ = (reg, off) => 0xb5000000 | ((off & 0x7ffff) << 5) | reg;
const TBZ = (reg, bit, off) =>
  0x36000000 | ((off & 0x3fff) << 5) | ((bit >> 5) << 31) | ((bit & 0x1f) << 19) | reg;
const SUBS_I = (reg, imm) => 0xf1100000 | ((imm & 0xfff) << 10) | (reg << 5) | reg;

// build(addr, words) -> returns [{name, x0, expectX1, count, desc}]
function scenario(name, x0, expectX1, code, count = 64, extra = {}) {
  return { name, x0, expectX1, code, count, extra };
}

const BASE = 0x90000;
const scenarios = [];

// --- pure ops sanity ---
scenarios.push(
  scenario('movz x1,#0x2a', 0, 0x2a, [MOVZ(1, 0x2a), HALT], 8),
  scenario('cmp #13 -> no crash (x0=13)', 13, undefined, [CMP(13), HALT], 8),
  scenario('subs x1,x1,#1 alone', 0, undefined, [SUBS_I(1, 1), HALT], 8),
  scenario('B(2) forward skip', 0, 7, [MOVZ(1, 3), B(2), MOVZ(1, 7), HALT], 16),
);

// --- b.eq: clean split, x1=1 if taken ---
{
  // 0x90000: cmp   x0,#13
  // 0x90004: b.eq  +3          -> 0x90010
  // 0x90008: movz  x1,#0
  // 0x9000c: b     .           (fall-through park)
  // 0x90010: movz  x1,#1
  // 0x90014: b     .           (taken park)
  const code = [CMP(13), BEQ(3), MOVZ(1, 0), HALT, MOVZ(1, 1), HALT];
  scenarios.push(scenario('b.eq taken  (x0=13)', 13, 1, code, 32));
  scenarios.push(scenario('b.eq not-taken (x0=1)', 1, 0, code, 32));
}

// --- b.ne: clean split ---
{
  // 0x90000: cmp   x0,#0
  // 0x90004: b.ne  +3          -> 0x90010
  // 0x90008: movz  x1,#0
  // 0x9000c: b     .
  // 0x90010: movz  x1,#1
  // 0x90014: b     .
  const code = [CMP(0), BNE(3), MOVZ(1, 0), HALT, MOVZ(1, 1), HALT];
  scenarios.push(scenario('b.ne taken  (x0=5)', 5, 1, code, 32));
  scenarios.push(scenario('b.ne not-taken (x0=0)', 0, 0, code, 32));
}

// --- cbnz: clean split ---
{
  // 0x90000: cbnz  x0,+3       -> 0x90010
  // 0x90004: movz  x1,#0
  // 0x90008: b     .
  // 0x9000c: (unused)
  // 0x90010: movz  x1,#1
  // 0x90014: b     .
  const code = [CBNZ(0, 3), MOVZ(1, 0), HALT, HALT, MOVZ(1, 1), HALT];
  scenarios.push(scenario('cbnz taken  (x0=5)', 5, 1, code, 32));
  scenarios.push(scenario('cbnz not-taken (x0=0)', 0, 0, code, 32));
}

// --- tbz ---
{
  // 0x90000: tbz   x0,#0,+3     -> 0x90010
  // 0x90004: movz  x1,#0
  // 0x90008: b     .
  // 0x9000c: (unused)
  // 0x90010: movz  x1,#1
  // 0x90014: b     .
  const code = [TBZ(0, 0, 3), MOVZ(1, 0), HALT, HALT, MOVZ(1, 1), HALT];
  scenarios.push(scenario('tbz taken  (x0=0, bit0=0)', 0, 1, code, 32));
  scenarios.push(scenario('tbz not-taken (x0=1, bit0=1)', 1, 0, code, 32));
}

// --- tbnz ---
{
  // 0x90000: tbnz  x0,#0,+3     -> 0x90010  (enc: tbnz = 0x37...)
  const tbnz = 0x37000000 | ((3 & 0x3fff) << 5) | ((0 >> 5) << 31) | ((0 & 0x1f) << 19) | 0;
  const code = [tbnz, MOVZ(1, 0), HALT, HALT, MOVZ(1, 1), HALT];
  scenarios.push(scenario('tbnz taken  (x0=1, bit0=1)', 1, 1, code, 32));
  scenarios.push(scenario('tbnz not-taken (x0=0, bit0=0)', 0, 0, code, 32));
}

// --- loop with big cap ---
{
  // 0x90000: subs  x0,x0,#1
  // 0x90004: b.ne  -2           -> 0x90000
  // 0x90008: movz  x1,#0x2a
  // 0x9000c: b     .
  const code = [SUBS_I(0, 1), BNE(-2), MOVZ(1, 0x2a), HALT];
  scenarios.push(scenario('loop 3x (subs;b.ne -2)', 3, 0x2a, code, 4096));
}

const results = { pass: 0, fail: 0, crash: 0 };
for (const s of scenarios) {
  const ucMod = await MUnicorn();
  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  uc.mem_map(0, 0x400000, ucMod.PROT_ALL);
  const bytes = s.code.flatMap(w);
  for (let i = 0; i < bytes.length; i++) uc.mem_write(BASE + i, [bytes[i]]);
  uc.reg_write_i32(ucMod.ARM64_REG_PC, BASE);
  uc.reg_write_i32(ucMod.ARM64_REG_X0, s.x0);
  let verdict = 'CRASH';
  try {
    uc.emu_start(BASE, 0, 0, s.count);
    const x1 = Number(uc.reg_read_i32(ucMod.ARM64_REG_X1));
    const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC));
    verdict = x1 === s.expectX1 ? 'PASS' : 'FAIL';
    if (verdict === 'FAIL') {
      console.log(`FAIL  ${s.name}: x1=${x1} pc=0x${pc.toString(16)} want x1=${s.expectX1}`);
    }
  } catch (e) {
    console.log(`CRASH ${s.name}: ${String(e.message || e).slice(0, 60)}`);
  }
  if (verdict === 'PASS') {
    results.pass++;
    console.log(`PASS  ${s.name}`);
  } else if (verdict === 'FAIL') results.fail++;
  else results.crash++;
  uc.close();
}
console.log(`--- ${results.pass} pass / ${results.fail} fail / ${results.crash} crash`);
process.exit(0);