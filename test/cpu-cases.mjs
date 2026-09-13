// Decoder golden test: every word comes from aarch64-none-elf-as
// (ground-truth encodings — never hand-hex). Each word runs on pi-cpu
// `one` with identical regs/flags/mem, comparing fault + all regs +
// full Q regs + NZCV + scratch windows against checked-in goldens
// (test/goldens/cpu-cases.json). The oracle differential this descends
// from (vs unicorn.js) is archived at test/archive/cpu-cases-oracle.mjs;
// correctness was proven there (756/756 + 35/35 SIMD rows) before the
// unicorn removal. Goldens pin the proven behavior against regressions.
// Usage: node test/cpu-cases.mjs [filter] [snipfilter] [--regen]
import { execFileSync, execSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');
const GOLD = join(__dirname, 'goldens', 'cpu-cases.json');
const TC = process.env.TOOLCHAIN || (process.env.HOME + '/toolchains/arm-gnu-toolchain-13.2.Rel1-x86_64-aarch64-none-elf/bin');

// ---- case list: groups of assembly snippets (one insn each) ----
const GROUPS = {
  logicalN: [
    'bic w0, w1, w2', 'bic x3, x4, x5', 'bic w6, w7, w8, lsl #5',
    'bic x9, x10, x11, lsr #9', 'orn x12, x13, x14', 'orn w15, w16, w17, asr #4',
    'eon x18, x19, x20, ror #11', 'eon w21, w22, w23',
    'bics w24, w25, w26', 'bics x27, x28, x29',
  ],
  extend: [
    'add x0, x1, w2, uxtw', 'add x3, x4, w5, uxtw #2', 'add x6, sp, w7, uxtx',
    'sub w8, w9, w10, sxtb', 'sub x11, x12, w13, sxth #1', 'sub x14, x15, x16, sxtx #3',
    'adds x17, x18, w19, uxtx #4', 'subs w20, w21, w22, sxtw',
    'ldr x0, [x1, w2, sxtw #3]', 'ldr w3, [x4, w5, uxtw #2]', 'ldr w6, [x7, w8, sxtw]',
    'ldrh w9, [x10, w11, sxtw]', 'add x12, x13, w14, uxtb',
  ],
  mem: [
    'ldr x0, [x1, #0]', 'ldr x2, [x3, #256]', 'str x4, [x5, #1024]',
    'ldrb w6, [x7, #17]', 'strb w8, [x9, #33]', 'ldrh w10, [x11, #66]',
    'strh w12, [x13, #88]', 'ldr w14, [x15, #100]', 'str w16, [x17, #64]',
    'ldur x18, [x19, #-8]', 'stur x20, [x21, #-16]', 'ldur w22, [x23, #-4]',
    'ldr x0, [x1], #8', 'str x2, [x3], #-8', 'ldr x4, [x5, #8]!', 'str x6, [x7, #-8]!',
    'ldp x0, x1, [x2]', 'stp x3, x4, [x5, #16]', 'ldp w6, w7, [x8, #12]',
    'stp w9, w10, [x11], #8', 'ldp x12, x13, [x14, #-16]!', 'ldpsw x15, x16, [x17]',
    'ldrsw x18, [x19, #24]', 'ldrsb x20, [x21, #8]', 'ldrsh w22, [x23, #10]',
    'ldrb w24, [x25, x26]', 'str x27, [x28, x29, lsl #3]',
    'ldr x0, [sp, #16]', 'str x1, [sp, #-8]!', 'ldp x2, x3, [sp], #16',
    'neg x0, x1', 'neg x2, x3', 'sub x4, xzr, x5', 'subs x6, xzr, x7',
    'add x8, sp, x9', 'adds x10, sp, x11',
    'stnp x0, x1, [x2]', 'ldnp x3, x4, [x5, #16]', 'stnp w6, w7, [x8]',
    'ldpsw x9, x10, [x11, #16]', 'ldpsw x12, x13, [x14], #8', 'ldpsw x15, x16, [x17, #-8]!',
    'ldrh w18, [x19, #8190]', 'ldr x20, [x21, #32760]', 'stur x22, [x23, #-256]',
  ],
  arith: [
    'adds x0, x1, x2', 'subs w3, w4, w5', 'adcs x6, x7, x8', 'sbcs w9, w10, w11',
    'adc x12, x13, x14', 'sbc x15, x16, x17', 'ngc x18, x19', 'ngcs w20, w21',
    'cmp x22, x23', 'cmn w24, w25', 'tst x26, x27', 'negs x28, x29',
    'ccmp w0, w5, #4, ne', 'ccmp w2, #7, #4, ne', 'ccmn w1, w6, #8, eq',
    'ccmn w3, #9, #2, lt', 'ccmp x4, x5, #4, gt', 'ccmn x6, #10, #2, ge',
    'csel x4, x5, x6, eq', 'csel w7, w8, w9, ne', 'csinc x10, x11, x12, gt',
    'cset w13, lt', 'csneg x14, x15, x16, le',
  ],
  move: [
    'movz w0, #0x1234', 'movz x1, #0x5678, lsl #16', 'movz x2, #0x9abc, lsl #32',
    'movz x3, #0xdef0, lsl #48', 'movn w4, #0x111', 'movn x5, #0x222, lsl #16',
    'movk w6, #0x333', 'movk x7, #0x4444, lsl #32', 'mov x8, sp', 'mov sp, x9',
    'adrp x10, 0x1000', 'adrp x11, 0x2000',
  ],
  bitfield: [
    'bfi x0, x1, #4, #8', 'bfxil x2, x3, #4, #8', 'sbfiz x4, x5, #6, #10',
    'sbfx x6, x7, #8, #12', 'ubfiz w8, w9, #2, #6', 'ubfx w10, w11, #5, #9',
    'lsl x12, x13, #7', 'lsr w14, w15, #3', 'asr x16, x17, #11', 'ror x18, x19, #13',
    'bfm x20, x21, #28, #7', 'bfm x22, x23, #8, #40', 'ubfm w24, w25, #10, #4',
    'sbfm x26, x27, #60, #7',
  ],
  branch: [
    'b #16', 'bl #32', 'b.eq #20', 'b.ne #24', 'b.cs #12', 'b.cc #28',
    'b.mi #16', 'b.pl #20', 'b.vs #24', 'b.hi #12', 'b.ls #28', 'b.ge #16',
    'cbz x0, #12', 'cbnz w1, #16', 'tbz x2, #5, #20', 'tbnz w3, #11, #24',
    'ret', 'blr x4',
  ],
  mul: [
    'madd x0, x1, x2, x3', 'msub x4, x5, x6, x7', 'madd w8, w9, w10, w11',
    'smaddl x12, w13, w14, x15', 'smsubl x16, w17, w18, x19',
    'umaddl x20, w21, w22, x23', 'umsubl x24, w25, w26, x27',
    'smulh x28, x29, x30', 'umulh x0, x1, x2',
    'udiv x3, x4, x5', 'sdiv x6, x7, x8', 'udiv w9, w10, w11', 'sdiv w12, w13, w14',
    'lsl x15, x16, x17', 'lsr w18, w19, w20', 'asr x21, x22, x23', 'ror w24, w25, w26',
  ],
  sysreg: [
    'mrs x0, nzcv', 'msr nzcv, x1', 'nop', 'isb', 'dsb sy', 'dmb ish',
  ],
  onesrc: [
    'rbit x0, x1', 'rbit w2, w3', 'rev x4, x5', 'rev16 x6, x7', 'rev32 x8, x9',
    'rev w10, w11', 'clz x12, x13', 'clz w14, w15', 'cls x16, x17', 'cls w18, w19',
  ],
  fp: [
    'fmov d0, x1', 'fmov x2, d3', 'fmov s4, w5', 'fmov w6, s7',
    'fmov d8, d9', 'fmov s10, s11', 'fmov d12, #1.0', 'fmov s13, #2.0',
    'fmov d14, #-0.5', 'fmov s15, #31.0',
    'fadd d0, d1, d2', 'fadd s3, s4, s5', 'fsub d6, d7, d8', 'fsub s9, s10, s11',
    'fmul d12, d13, d14', 'fmul s15, s16, s17', 'fdiv d18, d19, d20', 'fdiv s21, s22, s23',
    'fneg d24, d25', 'fneg s26, s27', 'fabs d28, d29', 'fabs s30, s0',
    'fsqrt d1, d2', 'fsqrt s3, s4',
    'fcmp d5, d6', 'fcmp s7, s8', 'fcmpe d9, d10', 'fcmpe s11, s12',
    'fcmp d13, #0.0', 'fcmp s14, #0.0', 'fcmpe d15, #0.0',
    'fcmp d9, d9', 'fcmp s8, s8',
    'fdiv d2, d2, d2', 'fsub d0, d0, d0', 'fmul d7, d7, d0', 'fsqrt d10, d10',
    'fccmp d16, d17, #8, ne', 'fccmp s18, s19, #0, lt',
    'fcsel d20, d21, d22, gt', 'fcsel s23, s24, s25, eq',
    'scvtf d26, x27', 'scvtf s28, w29', 'scvtf s0, x1', 'ucvtf d2, x3', 'ucvtf s4, w5',
    'fcvtzs w6, d7', 'fcvtzs x8, s9', 'fcvtzu w10, d11', 'fcvtas x12, d13',
    'frintm d14, d15', 'frinti s16, s17', 'frinta d18, d19', 'frintz s20, s21',
    'frintp d22, d23', 'frintn d24, d25', 'frintx s26, s27',
    'fcvt s28, d29', 'fcvt d30, s0',
    'fmadd d1, d2, d3, d4', 'fmsub s5, s6, s7, s8',
    'fnmadd d9, d10, d11, d12', 'fnmsub s13, s14, s15, s16',
    'str d0, [x8, #16]', 'ldr d1, [x8, #16]', 'str s2, [x9]', 'ldr s3, [x9]',
    'stur d4, [x10, #-8]', 'ldur d5, [x10, #-8]', 'ldr d6, [x11, #8]!', 'str d7, [x12], #8',
    'stp d13, d14, [x15]', 'ldp d16, d17, [x18]',
    'stp s19, s20, [x21, #8]!', 'ldp s22, s23, [x24], #8',
    'str d25, [sp, #16]', 'ldr d26, [sp, #16]',
    'movi v0.2d, #0', 'movi v1.16b, #32', 'dup v2.4h, w3',
    'eor v4.8b, v5.8b, v6.8b', 'eor v7.16b, v8.16b, v9.16b',
    // Lane-extract faults on the fork too (fault-both pins — executing
    // these here would break parity; guests never take them).
    'mov h10, v11.h[0]', 'mov h12, v13.h[3]',
  ],
  // M49 firmware-float gap rows (all oracle-verified one by one in the
  // simd-oracle rig before the unicorn removal; goldens pin them now).
  simdguest: [
    'extr x7, x7, x0, #61', 'extr x0, x1, x2, #0',
    'movi d8, #0', 'fmov x1, v0.d[1]', 'fmov v0.d[1], x3',
    'mov v0.16b, v1.16b', 'mov v2.8b, v0.8b',
    'bit v0.8b, v1.8b, v2.8b', 'bif v0.8b, v1.8b, v2.8b',
    'fneg v0.2d, v0.2d', 'mov v2.d[1], v0.d[0]',
    'scvtf d0, d0', 'fcvtzs d1, d1',
    'scvtf d0, d0, #1', 'scvtf d0, d0, #31', 'ucvtf d0, d0, #1',
    'fcvtzs w1, d1, #1', 'fcvtzs x2, d2, #63', 'fcvtzu w3, d3, #1',
    'shl d0, d0, #32', 'sshr d1, d1, #1',
    // M51 S-fixed + bit-select + dup/ushr rows (every word from
    // aarch64-none-elf-as; verified against the stock-unicorn oracle in
    // /tmp/opencode/verify51.mjs (values + Q tops) and verify-flags.mjs
    // (values + FPSR, incl. adversarial single-rounding sweep) before
    // pinning here. Notes: S-fixed int side is the low S32 (high D/Q
    // bits ignored); `ucvtf s0, s0` plain needed its own row (0x7E21D800);
    // BSL is spec-order (Vm selector) — the stock oracle computes
    // (Rn&Rd)|(Rm&~Rd) there (truth-table-proven oracle bug), so BSL
    // goldens pin the architecture behavior, grounded via oracle BIT;
    // DUP-from-XZR verified (=0) but excluded from the fuzzer per plan.
    'scvtf s0, s0, #1', 'scvtf s0, s0, #31', 'scvtf s0, s0, #32',
    'ucvtf s0, s0, #1', 'scvtf s0, s0', 'ucvtf s0, s0',
    'fcvtzs w0, s0, #1', 'fcvtzs w0, s0, #31',
    'fcvtzs x0, s0, #1', 'fcvtzs x0, s0, #63',
    'fcvtzu w0, s0, #1', 'fcvtzu x0, s0, #1',
    'bsl v0.8b, v1.8b, v2.8b', 'bsl v3.16b, v4.16b, v5.16b',
    'dup v0.2d, x0', 'dup v3.2d, x7',
    'ushr d0, d0, #1', 'ushr d0, d0, #63', 'ushr d0, d0, #64',
    'ushr v0.2d, v0.2d, #1', 'ushr v0.2d, v0.2d, #64',
  ],
};

// operand vectors: { regs: fn(i)->hex, sp, nzcv }
// V0 small ints; V1 addresses in scratch (valid memory bases); V2 msb-heavy.
function f64hex(x) {
  const b = Buffer.alloc(8);
  b.writeDoubleLE(x);
  return '0x' + b.toString('hex').match(/../g).reverse().join('');
}
const FD_PATTERNS = [
  // V0: ordinary doubles (exact + fractional + tiny).
  [1.5, -2.25, 0.0, -0.0, 3.14159, 100.25, 0.1, 2.5, -0.75, 1e10, 1e-10, 123456.789, -987.125, 0.5, 7.0, -16.5],
  // V1: bit patterns doubling as doubles (mostly normal values).
  null,
  // V2: extremes (infs, NaNs incl. signaling, max, min-subnormal, 0/0-adjacent).
  ['0x7ff0000000000000', '0xfff0000000000000', '0x7ff8000000000000', '0xfff8000000000001', '0x7ff0000000000001', '0x7fffffffffffffff', '0x0000000000000001', '0x8000000000000000', '0x0000000000000000', '0x3ff0000000000000', '0xbff0000000000000', '0x7fefffffffffffff', '0x0010000000000000', '0xfff8000000000000', '0x7ff4000000000000', '0xc000000000000000'],
];
const VECTORS = [
  { sp: '0x2000', nzcv: '0000', r: (i) => '0x' + ((i * 2654435761) % 97).toString(16), fd: (i) => f64hex(FD_PATTERNS[0][i % 16]) },
  {
    sp: '0x2000', nzcv: '0000',
    r: (i) => (i >= 8 && i <= 28) ? '0x' + (0x1000 + (i - 8) * 64).toString(16) : '0x' + ((i * 2654435761) % 97).toString(16),
    fd: (i) => (i >= 8 && i <= 28) ? '0x' + (0x1000 + (i - 8) * 64).toString(16) : '0x' + ((i * 2654435761) % 97).toString(16),
  },
  {
    sp: '0x3fff00', nzcv: '1010',
    r: (i) => (i >= 8 && i <= 28) ? '0x' + (0x3fff00 - (28 - i) * 64).toString(16) : '0x80000000000000' + (i % 16).toString(16),
    fd: (i) => FD_PATTERNS[2][i % 16],
  },
];

const filter = (process.argv[2] && !process.argv[2].startsWith('--')) ? process.argv[2] : '';
const snipfilter = (process.argv[3] && !process.argv[3].startsWith('--')) ? process.argv[3] : '';
const REGEN = process.argv.includes('--regen');
// assemble all snippets in one file
let asm = '.text\n';
const names = [];
for (const [g, list] of Object.entries(GROUPS)) {
  if (filter && g !== filter) continue;
  list.forEach((s, i) => {
    if (snipfilter && !s.includes(snipfilter)) return;
    asm += `${g}_${i}: ${s}\n`;
    names.push([g, i, s]);
  });
}
writeFileSync('/tmp/opencode/cases.s', asm);
execSync(`${TC}/aarch64-none-elf-as /tmp/opencode/cases.s -o /tmp/opencode/cases.o`);
const dis = execSync(`${TC}/aarch64-none-elf-objdump -d /tmp/opencode/cases.o`).toString();
// Key words by LABEL (never by position — address sorting once bit us).
const words = {};
let cur = null;
for (const line of dis.split('\n')) {
  const lab = line.match(/^[0-9a-f]+ <([A-Za-z0-9_]+)>:$/);
  if (lab) { cur = lab[1]; continue; }
  const m = line.match(/^\s+[0-9a-f]+:\s+([0-9a-f]{8})\s/);
  if (m && cur && !(cur in words)) words[cur] = m[1];
}
console.log(`assembled ${names.length} snippets, ${Object.keys(words).length} words`);

const w4 = (x) => Buffer.from([x & 0xff, (x >>> 8) & 0xff, (x >>> 16) & 0xff, (x >>> 24) & 0xff]);
const w8 = (x) => { const o = Buffer.alloc(8); let v = BigInt(x); for (let i = 0; i < 8; i++) { o[i] = Number(v & 0xffn); v >>= 8n; } return o; };
const PAT = [];
for (let i = 0; i < 16; i++) PAT.push(...w4((0x01020304 + i * 0x11111111) >>> 0));

const hx = (s) => { try { return BigInt(s.startsWith('0x') ? s : '0x' + s).toString(16); } catch { return s; } };
// Normalized comparison record: fault + X + D-lows + FULL Q (lane ops
// only move the top half) + SP + PC + NZCV + scratch windows.
const norm = (o) => JSON.stringify([o.fault, o.r.map(hx), (o.fp || []).map(hx),
  (o.q || []).map(hx), hx(o.sp), hx(o.pc), o.nzcv, o.mem]);

function runOne(word, regs, fds, sp, nzcv) {
  const args = ['0x' + word.toString(16),
    ...regs.map((x) => '0x' + x.toString(16)), sp, nzcv,
    ...fds.map((x) => '0x' + x.toString(16))];
  const out = execFileSync(ROOT + '/target/debug/examples/one', args, { maxBuffer: 8 * 1024 * 1024 }).toString();
  const L = Object.fromEntries(out.trim().split('\n').map((l) => {
    const j = l.indexOf(' ');
    return [l.slice(0, j), l.slice(j + 1)];
  }));
  return {
    fault: L['status'] !== 'ok',
    r: L.regs.split(' '), fp: L.fpregs.split(' '), q: L.qregs.split(' '),
    sp: L.sp.split(' ')[0], pc: L.sp.split(' ')[2], nzcv: L.nzcv, mem: L.mem.split(' '),
  };
}

let pass = 0, fail = 0;
const fails = [];
const goldens = (REGEN || !existsSync(GOLD)) ? {} : JSON.parse(readFileSync(GOLD, 'utf8'));
const regenOut = {};
for (let n = 0; n < names.length; n++) {
  const [g, i, src] = names[n];
  const word = parseInt(words[`${g}_${i}`], 16) >>> 0;
  for (let v = 0; v < VECTORS.length; v++) {
    const key = `${g}_${i}_${v}`;
    const V = VECTORS[v];
    const regs = [];
    for (let k = 0; k < 31; k++) regs.push(BigInt(V.r(k)));
    const fds = [];
    for (let k = 0; k < 32; k++) fds.push(BigInt(V.fd(k)));
    let got;
    try {
      got = runOne(word, regs, fds, BigInt(V.sp), V.nzcv);
    } catch (e) { got = { fault: 'CRASH' }; }
    if (REGEN) {
      if (got.fault !== 'CRASH') regenOut[key] = got;
      pass++;
      continue;
    }
    const want = goldens[key];
    if (!want) { fail++; fails.push(`${g} ${src} [V${v}] word=${words[`${g}_${i}`]}\n  DIFF: no golden (regen)`); continue; }
    // fault pc: goldens store pi-cpu's pre-incremented pc; same build
    // convention both sides, so compare directly (the -4 adjustment the
    // oracle needed no longer applies).
    // branches single-stepped: a bad target faults only at the next
    // fetch — compare pc, not fault. Indirect branches (ret/blr) to odd
    // targets: rounded pc — compare regs/flags only there.
    if (g === 'branch') { want.fault = false; got.fault = false; }
    if (src.startsWith('ret') || src.startsWith('blr')) { want.pc = got.pc; }
    if (norm(want) === norm(got)) { pass++; }
    else {
      fail++;
      if (fails.length < 15) {
        const dl = [];
        if (want.fault !== got.fault) dl.push(`fault ${want.fault}/${got.fault}`);
        want.r.forEach((v, i) => { if (hx(v) !== hx(got.r[i])) dl.push(`x${i} ${hx(v)}/${hx(got.r[i])}`); });
        if (got.fp) want.fp.forEach((v, i) => { if (hx(v) !== hx(got.fp[i])) dl.push(`d${i} ${hx(v)}/${hx(got.fp[i])}`); });
        if (got.q) want.q.forEach((v, i) => { if (hx(v) !== hx(got.q[i])) dl.push(`q${i} ${hx(v)}/${hx(got.q[i])}`); });
        if (hx(want.sp) !== hx(got.sp)) dl.push(`sp ${hx(want.sp)}/${hx(got.sp)}`);
        if (hx(want.pc) !== hx(got.pc)) dl.push(`pc ${hx(want.pc)}/${hx(got.pc)}`);
        if (want.nzcv !== got.nzcv) dl.push(`nz ${want.nzcv}/${got.nzcv}`);
        want.mem.forEach((v, i) => { if (v !== got.mem[i]) dl.push(`mem${i} ${v.slice(0, 24)}.. vs ${got.mem[i].slice(0, 24)}..`); });
        fails.push(`${g} ${src} [V${v}] word=${words[`${g}_${i}`]}\n  DIFF: ${dl.join(' | ')}`);
      }
    }
  }
}
if (REGEN) {
  mkdirSync(join(__dirname, 'goldens'), { recursive: true });
  writeFileSync(GOLD, JSON.stringify(regenOut));
  console.log(`regenerated ${Object.keys(regenOut).length} goldens -> test/goldens/cpu-cases.json`);
}
console.log(`${pass} ok, ${fail} FAIL`);
for (const f of fails) console.log(f);
process.exit(fail ? 1 : 0);
