// Probe v4: flags via REGISTER-form subs (no immediates) -> csel data-flow logic.
import { createRequire } from 'module';
import { join } from 'path';

const require = createRequire(import.meta.url);
const MUnicorn = require(join(import.meta.dirname, '..', 'public', 'unicorn.js'));

const w = (x) => [x & 0xff, (x >> 8) & 0xff, (x >> 16) & 0xff, (x >> 24) & 0xff];
const HALT = 0x14000000;
const MOVZ = (reg, imm) => 0x52800000 | ((imm & 0xffff) << 5) | reg;
// subs rd, rn, rm (shifted-reg, shift=0)
const SUBS_R = (rd, rn, rm) => 0xeb000000 | (rm << 16) | (rn << 5) | rd;
const SUB_R = (rd, rn, rm) => 0xcb000000 | (rm << 16) | (rn << 5) | rd;
// csel rd, rn, rm, cond
const CSEL = (rd, rn, rm, cond) => 0x9a800000 | (rm << 16) | (cond << 12) | (rn << 5) | rd;

const BASE = 0x90000;
const results = { pass: 0, fail: 0, crash: 0 };

async function run(name, regs, code, want) {
  const ucMod = await MUnicorn();
  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  uc.mem_map(0, 0x400000, ucMod.PROT_ALL);
  const bytes = code.flatMap(w);
  for (let i = 0; i < bytes.length; i++) uc.mem_write(BASE + i, [bytes[i]]);
  uc.reg_write_i32(ucMod.ARM64_REG_PC, BASE);
  for (const [r, v] of Object.entries(regs)) uc.reg_write_i32(ucMod.ARM64_REG_X0 + Number(r), v);
  let verdict = 'CRASH';
  try {
    uc.emu_start(BASE, 0, 0, 64);
    const x = (r) => Number(uc.reg_read_i32(ucMod.ARM64_REG_X0 + r));
    const got = want.map(([r]) => x(r));
    verdict = got.every((v, i) => v === want[i][1]) ? 'PASS' : 'FAIL';
    if (verdict === 'FAIL') console.log(`FAIL  ${name}: got [${got.join(',')}] want [${want.map((x) => x.join(':')).join(',')}]`);
    else results.pass++;
  } catch (e) {
    console.log(`CRASH ${name}: ${String(e.message || e).slice(0, 55)}`);
  }
  if (verdict === 'PASS') console.log(`PASS  ${name}`);
  else if (verdict === 'FAIL') results.fail++;
  else results.crash++;
  uc.close();
}

// 1. cmp-reg x1,x2 (5==5): csel x3,x4,x5,eq -> x4=7
await run('cmp-reg Z=1 -> csel eq -> x4', { 1: 5, 2: 5, 4: 7, 5: 9 }, [SUBS_R(31, 1, 2), CSEL(3, 4, 5, 0), HALT], [[3, 7]]);
// 2. cmp-reg (5!=7): Z=0 -> csel eq -> x5=9
await run('cmp-reg Z=0 -> csel eq -> x5', { 1: 5, 2: 7, 4: 7, 5: 9 }, [SUBS_R(31, 1, 2), CSEL(3, 4, 5, 0), HALT], [[3, 9]]);
// 3. cmp-reg (5!=7): N=1 -> csel lt -> x4
await run('cmp-reg N=1 -> csel lt -> x4', { 1: 5, 2: 7, 4: 7, 5: 9 }, [SUBS_R(31, 1, 2), CSEL(3, 4, 5, 0xb), HALT], [[3, 7]]);
// 4. subs x1,x1,x2 real reg (5-5=0): csel eq -> x4
await run('subs-reg result 0 -> csel eq', { 1: 5, 2: 5, 4: 7, 5: 9 }, [SUBS_R(1, 1, 2), CSEL(3, 4, 5, 0), HALT], [[1, 0], [3, 7]]);
// 5. subs x1,x1,x2 (5-7=-2): result -2, N=1 -> csel lt -> x4
await run('subs-reg -2 -> csel lt', { 1: 5, 2: 7, 4: 7, 5: 9 }, [SUBS_R(1, 1, 2), CSEL(3, 4, 5, 0xb), HALT], [[1, -2], [3, 7]]);
// 6. full uppercase-ish transform: c=97('a'), c>=97? c-32 else c  (csel ge)
//    movz x1,#0x61; movz x2,#0x61; movz x3,#0x20; subs xzr,x1,x2; sub w4,w1,w3; csel w5,w4,w1,ge
await run('lowercase->upper csel chain (a->A)', { 1: 0x61, 2: 0x61, 3: 0x20 }, [SUBS_R(31, 1, 2), SUB_R(4, 1, 3), CSEL(5, 4, 1, 0xa), HALT], [[5, 0x41]]);
// 7. same with 'z' (0x7A) -> 0x5A
await run('lowercase->upper (z->Z)', { 1: 0x7a, 2: 0x61, 3: 0x20 }, [SUBS_R(31, 1, 2), SUB_R(4, 1, 3), CSEL(5, 4, 1, 0xa), HALT], [[5, 0x5a]]);
// 8. same with '0' (0x30) -> NOT >= 'a' -> unchanged 0x30
await run('non-letter unchanged (0->0)', { 1: 0x30, 2: 0x61, 3: 0x20 }, [SUBS_R(31, 1, 2), SUB_R(4, 1, 3), CSEL(5, 4, 1, 0xa), HALT], [[5, 0x30]]);

console.log(`--- ${results.pass} pass / ${results.fail} fail / ${results.crash} crash`);
process.exit(0);