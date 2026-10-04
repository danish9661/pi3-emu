// SIMD differential rig (in-house, no unicorn): assemble each snippet,
// run pi-cpu cpu/examples/one.rs with seeded X/Q/scratch memory, and
// diff against an INDEPENDENT JS spec implementation of the ARM ARM
// semantics (documented inline per family). Catches semantics bugs in
// the new SIMD rows (M78/M79) that the self-consistency goldens cannot.
// Usage: node test/simd-diff.mjs [filter]
import { execFileSync } from 'node:child_process';
import { mkdtempSync, writeFileSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

const TC = process.env.TOOLCHAIN || (process.env.HOME + '/toolchains/arm-gnu-toolchain-13.2.Rel1-x86_64-aarch64-none-elf/bin');
const ONE = 'target/release/examples/one';

// ---- assembler (same recipe as cpu-cases.mjs) ----
function assemble(snippets) {
  const d = mkdtempSync(join(tmpdir(), 'simddiff-'));
  writeFileSync(join(d, 'c.s'), snippets.map((s, i) => `.global L${i}\nL${i}: ${s}`).join('\n') + '\n');
  execFileSync(`${TC}/aarch64-none-elf-as`, ['-o', join(d, 'c.o'), join(d, 'c.s')]);
  const out = execFileSync(`${TC}/aarch64-none-elf-objdump`, ['-d', join(d, 'c.o')]).toString();
  const words = [];
  for (const line of out.split('\n')) {
    const m = line.match(/^\s*[0-9a-f]+:\s+([0-9a-f]{8})\s/);
    if (m) words.push(parseInt(m[1], 16));
  }
  if (words.length !== snippets.length) throw new Error(`assemble: ${words.length} words != ${snippets.length} snippets`);
  return words;
}

// ---- one.rs runner ----
function runOne(word, x, sp, nzcv, q) {
  const args = [word.toString(16).padStart(8, '0'), ...x.map(v => v.toString(16)), sp.toString(16), nzcv];
  for (let i = 0; i < 32; i++) args.push((q[i] ?? 0n).toString(16));
  const out = execFileSync(ONE, args, { maxBuffer: 1 << 20 }).toString();
  const r = {};
  for (const line of out.split('\n')) {
    if (line.startsWith('status ')) r.fault = line !== 'status ok';
    if (line.startsWith('regs ')) r.x = line.slice(5).split(' ').map(h => BigInt('0x' + h));
    if (line.startsWith('qregs ')) r.q = line.slice(6).split(' ').map(h => BigInt('0x' + h));
    if (line.startsWith('sp ')) { const p = line.split(' '); r.sp = BigInt('0x' + p[1]); r.pc = BigInt('0x' + p[3]); }
    if (line.startsWith('mem ')) r.mem = line.slice(4).split(' ').map(h => Buffer.from(h, 'hex'));
  }
  return r;
}

// ---- shared scratch memory model (mirrors one.rs) ----
function seedScratch() {
  const mem = new Map(); // byte-addressed
  const put4 = (addr, v) => { for (let i = 0; i < 4; i++) mem.set(addr + i, Number((v >> BigInt(8 * i)) & 0xFFn)); };
  for (let i = 0; i < 16; i++) put4(0x1000 + i * 4, (0x01020304n + BigInt(i) * 0x11111111n) & 0xFFFFFFFFn);
  for (let i = 0; i < 8; i++) put4(0x2000 + i * 4, (0x01020304n + BigInt(i) * 0x11111111n) & 0xFFFFFFFFn);
  for (let i = 0; i < 16; i++) put4(0x3fff00 - 64 + i * 4, (0x01020304n + BigInt(i) * 0x11111111n) & 0xFFFFFFFFn);
  return mem;
}
const M64 = (1n << 64n) - 1n, M128 = (1n << 128n) - 1n;
const slice = (q, lo, sz) => (q >> BigInt(lo)) & ((1n << BigInt(sz)) - 1n);

// ---- operand vectors ----
const QVECS = [
  () => new Array(32).fill(0n),
  i => Array.from({ length: 32 }, (_, j) => (BigInt(j * 7 + 1) * 0x0102030405060708n + BigInt(i * 13)) & M128),
  i => Array.from({ length: 32 }, (_, j) => (BigInt(j * 3 + i) * 0xFF7F000080810203n + (j % 2 ? 0x8000000000000000n : 0n)) & M128),
];

// ---- JS spec implementations ----
// Each takes (word, st) where st = {x, sp, q, mem}; returns {q?, x?, sp?, memWrites: Map}.
function lanes(q, esz, count) {
  const out = [];
  for (let i = 0; i < count; i++) out.push(slice(q, i * esz, esz));
  return out;
}
function dupCommon(word, st) {
  const Q = (word >>> 30) & 1, imm5 = (word >>> 16) & 0x1F, rn = (word >>> 5) & 0x1F;
  const k = Math.clz32(imm5 & 0x80000000) === 0 ? 0 : 31 - Math.clz32(imm5);
  // trailing zeros:
  let k2 = 0; while (k2 < 5 && ((imm5 >>> k2) & 1) === 0) k2++;
  const esz = 8 << k2;
  if (esz === 64 && Q === 0) return null;
  const src = st.x[rn] & (esz === 64 ? M64 : (1n << BigInt(esz)) - 1n);
  const cnt = (Q ? 128 : 64) / esz;
  let out = 0n;
  for (let i = 0; i < cnt; i++) out |= src << BigInt(i * esz);
  return out;
}
function dupElem(word, st) {
  const Q = (word >>> 30) & 1, C = (word >>> 16) & 0xFF, rn = (word >>> 5) & 0x1F;
  let k = 0; while (k < 8 && ((C >>> k) & 1) === 0) k++;
  const esz = 8 << k, idx = C >>> (k + 1);
  if (esz === 64 && Q === 0) return null;
  const v = slice(st.q[rn], idx * esz, esz);
  const cnt = (Q ? 128 : 64) / esz;
  let out = 0n;
  for (let i = 0; i < cnt; i++) out |= v << BigInt(i * esz);
  return out;
}
function cmpZero(word, st) {
  const Q = (word >>> 30) & 1, S = (word >>> 29) & 1, opc = (word >>> 12) & 0xF;
  const rn = (word >>> 5) & 0x1F;
  const kind = (opc === 9 && S === 0) ? 0 : (opc === 9 && S === 1) ? 1 : (opc === 8 && S === 0) ? 2 : (opc === 8 && S === 1) ? 3 : (opc === 10 && S === 0) ? 4 : -1;
  if (kind < 0) return null;
  const esz = 8 << ((word >>> 22) & 3);
  const cnt = (Q ? 128 : 64) / esz;
  const lmask = esz === 64 ? M64 : (1n << BigInt(esz)) - 1n;
  let out = 0n;
  for (let i = 0; i < cnt; i++) {
    const lane = slice(st.q[rn], i * esz, esz);
    const sign = (lane >> BigInt(esz - 1)) & 1n;
    const t = kind === 0 ? lane === 0n : kind === 1 ? (sign === 1n || lane === 0n) : kind === 2 ? (sign === 0n && lane !== 0n) : kind === 3 ? sign === 0n : sign === 1n;
    if (t) out |= lmask << BigInt(i * esz);
  }
  return out;
}
function cmpTst(word, st) {
  const Q = (word >>> 30) & 1, rn = (word >>> 5) & 0x1F, rm = (word >>> 16) & 0x1F;
  const esz = 8 << ((word >>> 22) & 3);
  const cnt = (Q ? 128 : 64) / esz;
  const lmask = esz === 64 ? M64 : (1n << BigInt(esz)) - 1n;
  let out = 0n;
  for (let i = 0; i < cnt; i++) {
    const a = slice(st.q[rn], i * esz, esz), b = slice(st.q[rm], i * esz, esz);
    if ((a & b) !== 0n) out |= lmask << BigInt(i * esz);
  }
  return out;
}
function sextU(v, esz) { const b = BigInt(esz - 1); return (v & (1n << b)) !== 0n ? v - (1n << BigInt(esz)) : v; }
function cmp3(word, st) {
  const Q = (word >>> 30) & 1, S = (word >>> 29) & 1, opc = (word >>> 10) & 0x3F;
  const rn = (word >>> 5) & 0x1F, rm = (word >>> 16) & 0x1F;
  const kind = opc === 0x23 && S === 1 ? 0 : opc === 0x0F && S === 0 ? 1 : opc === 0x0D && S === 0 ? 2 : opc === 0x0D && S === 1 ? 3 : opc === 0x0F && S === 1 ? 4 : -1;
  if (kind < 0) return null;
  const esz = 8 << ((word >>> 22) & 3);
  const cnt = (Q ? 128 : 64) / esz;
  const lmask = esz === 64 ? M64 : (1n << BigInt(esz)) - 1n;
  let out = 0n;
  for (let i = 0; i < cnt; i++) {
    const a = slice(st.q[rn], i * esz, esz), b = slice(st.q[rm], i * esz, esz);
    const t = kind === 0 ? a === b : kind === 1 ? sextU(a, esz) >= sextU(b, esz) : kind === 2 ? sextU(a, esz) > sextU(b, esz) : kind === 3 ? a > b : a >= b;
    if (t) out |= lmask << BigInt(i * esz);
  }
  return out;
}
function logic3(word, st) {
  const Q = (word >>> 30) & 1, S = (word >>> 29) & 1, opc = (word >>> 10) & 0x3F;
  const rn = (word >>> 5) & 0x1F, rm = (word >>> 16) & 0x1F, rd = word & 0x1F;
  const w = Q ? M128 : M64;
  if (S === 0 && opc === 0x07 && ((word >>> 22) & 3) === 0) return st.q[rn] & st.q[rm] & w;          // AND
  if (S === 0 && opc === 0x07 && ((word >>> 22) & 3) === 1) return st.q[rn] & ~st.q[rm] & w;         // BIC
  return null;
}
function bitSel(word, st) {
  const Q = (word >>> 30) & 1, S = (word >>> 29) & 1, opc = (word >>> 10) & 0x3F;
  const rn = (word >>> 5) & 0x1F, rm = (word >>> 16) & 0x1F, rd = word & 0x1F;
  const w = Q ? M128 : M64;
  if (S === 1 && opc === 0x07 && ((word >>> 22) & 3) === 2) return ((st.q[rm] & st.q[rn]) | (st.q[rd] & ~st.q[rm])) & w; // BIT
  if (S === 1 && opc === 0x07 && ((word >>> 22) & 3) === 3) return ((st.q[rm] & st.q[rd]) | (st.q[rn] & ~st.q[rm])) & w; // BIF
  return null;
}
function pairwise(word, st) {
  const Q = (word >>> 30) & 1, S = (word >>> 29) & 1, opc = (word >>> 10) & 0x3F;
  const rn = (word >>> 5) & 0x1F, rm = (word >>> 16) & 0x1F;
  let kind = -1;
  if (S === 0 && opc === 0x2F) kind = 0;        // ADDP
  else if (S === 1 && opc === 0x29) kind = 1;   // UMAXP
  else if (S === 0 && opc === 0x29) kind = 2;   // SMAXP
  else if (S === 1 && opc === 0x2B) kind = 3;   // UMINP
  else if (S === 0 && opc === 0x2B) kind = 4;   // SMINP
  if (kind < 0) return null;
  const esz = 8 << ((word >>> 22) & 3);
  const cnt = (Q ? 128 : 64) / esz, half = cnt / 2;
  const lmask = esz === 64 ? M64 : (1n << BigInt(esz)) - 1n;
  const pick = (a, b) => kind === 0 ? (a + b) & lmask
    : kind === 1 ? (a >= b ? a : b)
    : kind === 2 ? (sextU(a, esz) >= sextU(b, esz) ? a : b)
    : kind === 3 ? (a <= b ? a : b)
    : (sextU(a, esz) <= sextU(b, esz) ? a : b);
  let out = 0n;
  for (let i = 0; i < half; i++) out |= pick(slice(st.q[rn], 2 * i * esz, esz), slice(st.q[rn], (2 * i + 1) * esz, esz)) << BigInt(i * esz);
  for (let j = 0; j < half; j++) out |= pick(slice(st.q[rm], 2 * j * esz, esz), slice(st.q[rm], (2 * j + 1) * esz, esz)) << BigInt((half + j) * esz);
  return out;
}
function umov(word, st, signed) {
  const Q = (word >>> 30) & 1, C = (word >>> 16) & 0xFF, rn = (word >>> 5) & 0x1F, rd = word & 0x1F;
  let k = 0; while (k < 8 && ((C >>> k) & 1) === 0) k++;
  const esz = 8 << k, idx = C >>> (k + 1);
  if (!signed) {
    if (Q === 0 && esz === 64) return null;
    if (Q === 1 && esz !== 64) return null;
  } else {
    if (esz === 64) return null;
    if (Q === 0 && esz === 32) return null;
  }
  const v = slice(st.q[rn], idx * esz, esz);
  if (rd === 31) return { skip: true };
  if (!signed) return { x: [rd, v & (esz === 64 ? M64 : (1n << BigInt(esz)) - 1n)] };
  const s = sextU(v, esz);
  return { x: [rd, Q ? (s & M64) : (s & 0xFFFFFFFFn)] };
}
function moviFamily(word, st) {
  const Q = (word >>> 30) & 1, op = (word >>> 29) & 1, cmode = (word >>> 12) & 0xF;
  const grp = cmode & 1, ss = (cmode >> 1) & 7, rd = word & 0x1F;
  const imm8 = BigInt((((word >>> 16) & 7) << 5) | ((word >>> 5) & 31));
  const width = Q ? M128 : M64;
  let cls, esz, sh;
  if (grp === 0 && ss === 7) { if (op === 0) { cls = 0; esz = 8; sh = 0; } else { cls = 4; esz = 64; sh = 0; } }
  else if (grp === 0 && (ss === 4 || ss === 5)) { cls = op ? 1 : 0; esz = 16; sh = (ss - 4) * 8; }
  else if (grp === 0 && ss <= 3) { cls = op ? 1 : 0; esz = 32; sh = ss * 8; }
  else if (grp === 1 && ss <= 3) { cls = op ? 3 : 2; esz = 32; sh = ss * 8; }
  else if (grp === 1 && (ss === 4 || ss === 5)) { cls = op ? 3 : 2; esz = 16; sh = (ss - 4) * 8; }
  else return null;
  let pat;
  if (cls === 4) {
    pat = 0n;
    for (let i = 0; i < 8; i++) if ((imm8 >> BigInt(i)) & 1n) pat |= 0xFFn << BigInt(i * 8);
    if (Q) pat |= pat << 64n;
  } else {
    const elem = (imm8 << BigInt(sh)) & (esz === 16 ? 0xFFFFn : 0xFFFFFFFFn);
    pat = 0n;
    for (let i = 0; i < 128 / esz; i++) pat |= elem << BigInt(i * esz);
    pat &= width;
  }
  return cls === 0 || cls === 4 ? pat : cls === 1 ? ~pat & width : cls === 2 ? (st.q[rd] | pat) & width : (st.q[rd] & ~pat) & width;
}

// ---- memory ops: LD1/ST1 + LDST Q/B reg-offset ----
function readMem(mem, addr, n) { addr = BigInt(addr); let v = 0n; for (let i = 0n; i < BigInt(n); i++) v |= BigInt(mem.get(Number(addr + i)) ?? 0) << BigInt(8 * Number(i)); return v; }
function writeMem(mem, addr, v, n) { addr = BigInt(addr); for (let i = 0n; i < BigInt(n); i++) mem.set(Number(addr + i), Number((v >> BigInt(8 * Number(i))) & 0xFFn)); }
function ld1st1(word, st) {
  const L = (word >>> 22) & 1, R = (word >>> 23) & 1, Q = (word >>> 30) & 1;
  const opc = (word >>> 12) & 0xF;
  const count = opc === 7 ? 1 : opc === 10 ? 2 : opc === 6 ? 3 : opc === 2 ? 4 : -1;
  if (count < 0) return null;
  const per = Q ? 16 : 8, total = count * per;
  const rm = (word >>> 16) & 31, rn = (word >>> 5) & 31, rt = word & 31;
  const base = rn === 31 ? st.sp : st.x[rn];
  let addr = base;
  const q = [...st.q], memWrites = [];
  for (let i = 0; i < count; i++) {
    const r = (rt + i) & 31;
    if (L) q[r] = readMem(st.mem, addr, per);
    else { writeMem(st.mem, addr, q[r], per); memWrites.push([addr, per]); }
    addr += BigInt(per);
  }
  let sp = st.sp, xs = [];
  if (R) {
    const inc = rm === 31 ? BigInt(total) : st.x[rm];
    if (rn === 31) sp = (base + inc) & M64; else xs.push([rn, (base + inc) & M64]);
  }
  return { q, xs, sp, memWrites };
}
function ldstQ(word, st) {
  const bit23 = (word >>> 23) & 1, opc = (word >>> 22) & 3;
  const esz = bit23 ? 16 : 1, isLoad = (opc & 1) === 1;
  const rm = (word >>> 16) & 31, rn = (word >>> 5) & 31, rt = word & 31;
  const bit12 = (word >>> 12) & 1;
  const base = rn === 31 ? st.sp : st.x[rn];
  let addr;
  if ((word >>> 24) & 1) addr = base + BigInt(((word >>> 10) & 0xFFF) * 16);
  else if ((word >>> 21) & 1) addr = base + ((st.x[rm] << BigInt(bit12 ? (esz === 16 ? 4 : 0) : 0)) & M64);
  else return null; // unscaled/pre/post not in scope here
  const q = [...st.q], memWrites = [];
  if (isLoad) q[rt] = readMem(st.mem, Number(addr), esz);
  else { writeMem(st.mem, Number(addr), q[rt], esz); memWrites.push([Number(addr), esz]); }
  return { q, memWrites };
}

// ---- case tables ----
const CASES = [
  ['dup v0.8b, w1', dupCommon, 0], ['dup v1.16b, w2', dupCommon, 0], ['dup v2.8h, w3', dupCommon, 0],
  ['dup v4.2s, w5', dupCommon, 0], ['dup v5.4s, w6', dupCommon, 0], ['dup v6.2d, x8', dupCommon, 0],
  ['dup v4.16b, v5.b[3]', dupElem, 0], ['dup v6.8h, v7.h[2]', dupElem, 0],
  ['dup v8.4s, v9.s[1]', dupElem, 0], ['dup v10.2d, v11.d[0]', dupElem, 0],
  ['cmeq v2.16b, v1.16b, #0', cmpZero, 0], ['cmeq v2.8b, v1.8b, #0', cmpZero, 0],
  ['cmeq v2.4h, v1.4h, #0', cmpZero, 0], ['cmeq v2.2s, v1.2s, #0', cmpZero, 0],
  ['cmeq v2.2d, v1.2d, #0', cmpZero, 0], ['cmge v2.16b, v1.16b, #0', cmpZero, 0],
  ['cmgt v2.16b, v1.16b, #0', cmpZero, 0], ['cmlt v2.16b, v1.16b, #0', cmpZero, 0],
  ['cmle v2.16b, v1.16b, #0', cmpZero, 0], ['cmge v2.4h, v1.4h, #0', cmpZero, 0],
  ['cmgt v2.2s, v1.2s, #0', cmpZero, 0], ['cmtst v2.16b, v1.16b, v3.16b', cmpTst, 0],
  ['cmtst v2.8b, v1.8b, v3.8b', cmpTst, 0], ['cmtst v2.2d, v1.2d, v3.2d', cmpTst, 0],
  ['cmeq v3.16b, v1.16b, v0.16b', cmp3, 0], ['cmge v3.16b, v1.16b, v0.16b', cmp3, 0],
  ['cmgt v3.16b, v1.16b, v0.16b', cmp3, 0], ['cmhi v3.16b, v1.16b, v0.16b', cmp3, 0],
  ['cmhs v3.16b, v1.16b, v0.16b', cmp3, 0], ['and v3.16b, v1.16b, v0.16b', logic3, 0],
  ['bic v3.16b, v1.16b, v0.16b', logic3, 0], ['cmeq v3.8b, v1.8b, v0.8b', cmp3, 0],
  ['cmeq v3.4h, v1.4h, v0.4h', cmp3, 0], ['cmeq v3.2s, v1.2s, v0.2s', cmp3, 0],
  ['cmeq v3.2d, v1.2d, v0.2d', cmp3, 0], ['cmge v3.4h, v1.4h, v0.4h', cmp3, 0],
  ['and v3.8b, v1.8b, v0.8b', logic3, 0],
  ['bit v0.16b, v1.16b, v2.16b', bitSel, 0], ['bif v0.16b, v1.16b, v2.16b', bitSel, 0],
  ['addp v5.16b, v2.16b, v2.16b', pairwise, 0], ['addp v5.8b, v2.8b, v2.8b', pairwise, 0],
  ['addp v5.8h, v2.8h, v2.8h', pairwise, 0], ['addp v5.4s, v2.4s, v2.4s', pairwise, 0],
  ['addp v5.2d, v2.2d, v2.2d', pairwise, 0], ['addp v5.4h, v2.4h, v3.4h', pairwise, 0],
  ['umaxp v6.16b, v2.16b, v2.16b', pairwise, 0], ['smaxp v6.16b, v2.16b, v2.16b', pairwise, 0],
  ['uminp v6.16b, v2.16b, v2.16b', pairwise, 0], ['sminp v6.16b, v2.16b, v2.16b', pairwise, 0],
  ['umaxp v6.4s, v2.4s, v2.4s', pairwise, 0], ['sminp v6.8h, v2.8h, v2.8h', pairwise, 0],
  ['mov x3, v3.d[0]', (w, st) => umov(w, st, false), 0], ['umov w4, v5.b[7]', (w, st) => umov(w, st, false), 0],
  ['umov w4, v5.h[3]', (w, st) => umov(w, st, false), 0], ['umov w4, v5.s[1]', (w, st) => umov(w, st, false), 0],
  ['umov x6, v7.d[1]', (w, st) => umov(w, st, false), 0],
  ['smov x6, v7.b[0]', (w, st) => umov(w, st, true), 0], ['smov x6, v7.h[0]', (w, st) => umov(w, st, true), 0],
  ['smov w6, v7.b[0]', (w, st) => umov(w, st, true), 0], ['smov x6, v7.s[0]', (w, st) => umov(w, st, true), 0],
  ['smov w6, v7.h[2]', (w, st) => umov(w, st, true), 0],
  ['smov x8, v9.b[15]', (w, st) => umov(w, st, true), 0], ['smov x8, v9.s[3]', (w, st) => umov(w, st, true), 0],
  ['movi v0.8b, #0x20', moviFamily, 0], ['movi v0.16b, #0x20', moviFamily, 0],
  ['movi v0.4h, #0x20', moviFamily, 0], ['movi v0.4h, #0x20, lsl #8', moviFamily, 0],
  ['movi v0.8h, #0x20, lsl #8', moviFamily, 0], ['movi v0.2s, #0x20', moviFamily, 0],
  ['movi v0.2s, #0x20, lsl #24', moviFamily, 0], ['movi v0.2d, #0xff', moviFamily, 0],
  ['mvni v0.4s, #0x20', moviFamily, 0], ['mvni v0.2s, #0x20, lsl #8', moviFamily, 0],
  ['orr v0.4s, #0x20', moviFamily, 0], ['orr v0.2s, #0x20, lsl #16', moviFamily, 0],
  ['bic v0.8h, #0xf, lsl #8', moviFamily, 0], ['bic v0.4s, #0x20', moviFamily, 0],
  ['bic v0.2s, #0x20, lsl #8', moviFamily, 0],
];
const MEM_CASES = [
  ['ld1 {v1.16b}, [x8], #16', ld1st1, 0x1000], ['st1 {v1.16b}, [x8], #16', ld1st1, 0x1000],
  ['ld1 {v2.8b}, [x8], #8', ld1st1, 0x1000], ['st1 {v2.8b}, [x8], #8', ld1st1, 0x1000],
  ['ld1 {v3.4h}, [x8], #8', ld1st1, 0x1000], ['ld1 {v4.8h}, [x9], #16', ld1st1, 0x1000],
  ['ld1 {v5.2s}, [x8], #8', ld1st1, 0x1000], ['st1 {v5.4s}, [x9], #16', ld1st1, 0x1000],
  ['ld1 {v6.1d}, [x8], #8', ld1st1, 0x1000], ['ld1 {v7.2d}, [x9], #16', ld1st1, 0x1000],
  ['st1 {v0.2d}, [x8], #16', ld1st1, 0x1000],
  ['ld1 {v0.16b, v1.16b}, [x8], #32', ld1st1, 0x1000], ['st1 {v0.16b, v1.16b}, [x8], #32', ld1st1, 0x1000],
  ['ld1 {v2.8b, v3.8b}, [x9], #16', ld1st1, 0x1000],
  ['ld1 {v0.16b, v1.16b, v2.16b}, [x8], #48', ld1st1, 0x1000],
  ['ld1 {v0.16b, v1.16b, v2.16b, v3.16b}, [x8], #64', ld1st1, 0x1000],
  ['st1 {v0.16b, v1.16b, v2.16b, v3.16b}, [x9], #64', ld1st1, 0x1000],
  ['ld1 {v1.16b}, [x8]', ld1st1, 0x1000], ['st1 {v1.2d}, [x9]', ld1st1, 0x1000],
  ['ldr q1, [x1, x5]', ldstQ, 0x1000], ['str q1, [x0, x5]', ldstQ, 0x1000],
  ['ldr q2, [x1, x5, lsl #4]', ldstQ, 0x1000], ['str q3, [x2, x3, lsl #4]', ldstQ, 0x1000],
  ['ldr q0, [x1]', ldstQ, 0x1000], ['str q0, [x0]', ldstQ, 0x1000],
];

const filter = process.argv[2];
const allSnips = [...CASES.map(c => c[0]), ...MEM_CASES.map(c => c[0])];
const words = assemble(allSnips);
let pass = 0, fail = 0;
function bigEq(a, b) { return (a & M128) === (b & M128); }

for (let vi = 0; vi < 3; vi++) {
  const qv = QVECS[vi](vi);
  for (let ci = 0; ci < CASES.length; ci++) {
    const [snip, fn] = CASES[ci];
    if (filter && !snip.includes(filter)) continue;
    const word = words[ci];
    const x = Array.from({ length: 31 }, (_, i) => (BigInt(i * 0x11111111) + BigInt(vi * 0x5555)) & M64);
    const st = { x, sp: 0x3fff00n - 64n, q: [...qv], mem: new Map() };
    const want = fn(word, st);
    const got = runOne(word, x.map(Number), 0x3fff00 - 64, '0000', qv);
    if (want === null) { // should fault
      if (got.fault) { pass++; } else { fail++; console.log(`XFAIL(v${vi}) ${snip} [0x${word.toString(16)}] expected fault, executed`); }
      continue;
    }
    const rd = word & 31;
    let ok = !got.fault;
    if (ok) {
      if (want.x) {
        const [r, v] = want.x;
        if (!bigEq(got.x[r], v)) { ok = false; console.log(`FAIL(v${vi}) ${snip} [0x${word.toString(16)}] x${r} got 0x${got.x[r].toString(16)} want 0x${v.toString(16)}`); }
      } else if (!want.skip) {
        const w64 = want & M128;
        if (!bigEq(got.q[rd], w64)) { ok = false; console.log(`FAIL(v${vi}) ${snip} [0x${word.toString(16)}] q${rd} got 0x${got.q[rd].toString(16).padStart(32, '0')} want 0x${w64.toString(16).padStart(32, '0')}`); }
      }
    } else console.log(`FAIL(v${vi}) ${snip} [0x${word.toString(16)}] FAULT in pi-cpu`);
    ok ? pass++ : fail++;
  }
  for (let ci = 0; ci < MEM_CASES.length; ci++) {
    const [snip, fn, base] = MEM_CASES[ci];
    if (filter && !snip.includes(filter)) continue;
    const word = words[CASES.length + ci];
    const x = Array.from({ length: 31 }, (_, i) => (BigInt(i * 0x11111111) + BigInt(vi * 0x7777)) & M64);
    // point x8/x9/x0/x1/x2/x3/x5 at scratch
    x[0] = BigInt(base); x[1] = BigInt(base); x[2] = BigInt(base); x[3] = 0x20n; x[5] = 0x20n; x[8] = BigInt(base); x[9] = BigInt(base);
    const st = { x, sp: 0x3fff00n - 64n, q: [...qv], mem: seedScratch() };
    const want = fn(word, st);
    const got = runOne(word, x.map(Number), 0x3fff00 - 64, '0000', qv);
    if (want === null) { got.fault ? pass++ : (fail++, console.log(`XFAIL(v${vi}) ${snip} expected fault`)); continue; }
    // Apply the spec's updated state before comparing.
    if (want.q) st.q = want.q;
    if (want.xs) for (const [r, v] of want.xs) st.x[r] = v;
    if (want.sp !== undefined) st.sp = want.sp;
    let ok = !got.fault;
    if (ok) {
      for (let i = 0; i < 32; i++) {
        if (!bigEq(got.q[i], st.q[i])) { ok = false; console.log(`FAIL(v${vi}) ${snip} [0x${word.toString(16)}] q${i} got 0x${got.q[i].toString(16).padStart(32, '0')} want 0x${st.q[i].toString(16).padStart(32, '0')}`); break; }
      }
      // memory: compare the scratch windows
      if (ok) {
        const wins = got.mem;
        let mi = 0;
        for (const [wbase, wlen] of [[0x1000, 64], [0x2000, 32], [0x3fff00 - 64, 64]]) {
          for (let i = 0; i < wlen; i++) {
            const wantB = st.mem.get(wbase + i) ?? 0;
            const gotB = wins[mi][i];
            if (gotB !== wantB) { ok = false; console.log(`FAIL(v${vi}) ${snip} [0x${word.toString(16)}] mem[0x${(wbase + i).toString(16)}] got 0x${gotB.toString(16)} want 0x${wantB.toString(16)}`); break; }
          }
          mi++;
          if (!ok) break;
        }
      }
    } else console.log(`FAIL(v${vi}) ${snip} [0x${word.toString(16)}] FAULT in pi-cpu`);
    ok ? pass++ : fail++;
  }
}
console.log(`simd-diff: ${pass} ok, ${fail} FAIL`);
process.exit(fail ? 1 : 0);
