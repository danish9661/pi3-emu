// Decoder differential: every word comes from aarch64-none-elf-as
// (ground-truth encodings — never hand-hex). Each word runs once on the
// unicorn oracle and once on pi-cpu `one`, with identical regs/flags/mem,
// comparing fault + all regs + NZCV + scratch windows.
// Usage: node test/cpu-cases.mjs [filter]
import { execFileSync, execSync } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
const require = createRequire('/home/danish1075/Documents/ri pi emu/packages/pi3-emu/src/index.js');
const MUnicorn = require('/home/danish1075/Documents/ri pi emu/public/unicorn.js');
const ucMod = await MUnicorn();
const ROOT = '/home/danish1075/Documents/ri pi emu';
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
    'stnp x0, x1, [x2]', 'ldnp x3, x4, [x5, #16]', 'stnp w6, w7, [x8]',
    'ldpsw x9, x10, [x11, #16]', 'ldpsw x12, x13, [x14], #8', 'ldpsw x15, x16, [x17, #-8]!',
    'ldrh w18, [x19, #8190]', 'ldr x20, [x21, #32760]', 'stur x22, [x23, #-256]',
  ],
  arith: [
    'adds x0, x1, x2', 'subs w3, w4, w5', 'adcs x6, x7, x8', 'sbcs w9, w10, w11',
    'adc x12, x13, x14', 'sbc x15, x16, x17', 'ngc x18, x19', 'ngcs w20, w21',
    'cmp x22, x23', 'cmn w24, w25', 'tst x26, x27', 'negs x28, x29',
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
};

// operand vectors: { regs: fn(i)->hex, sp, nzcv }
// V0 small ints; V1 addresses in scratch (valid memory bases); V2 msb-heavy.
const VECTORS = [
  { sp: '0x2000', nzcv: '0000', r: (i) => '0x' + ((i * 2654435761) % 97).toString(16) },
  {
    sp: '0x2000', nzcv: '0000',
    r: (i) => (i >= 8 && i <= 28) ? '0x' + (0x1000 + (i - 8) * 64).toString(16) : '0x' + ((i * 2654435761) % 97).toString(16),
  },
  {
    sp: '0x3fff00', nzcv: '1010',
    r: (i) => (i >= 8 && i <= 28) ? '0x' + (0x3fff00 - (28 - i) * 64).toString(16) : '0x80000000000000' + (i % 16).toString(16),
  },
];

const filter = process.argv[2] || '';
const snipfilter = process.argv[3] || '';
// assemble all snippets in one file
let asm = '.text\n';
const names = [];
for (const [g, list] of Object.entries(GROUPS)) {
  if (filter && g !== filter) continue;
  list.forEach((s, i) => {
    if (snipfilter && !s.includes(snipfilter)) return;
    asm += `${g}_${names.length}: ${s}\n`;
    names.push([g, names.length, s]);
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

async function oracle(word, regs, sp, nzcv) {
  // Fresh instance per case (closed after): instance reuse caused
  // cross-case state pollution (a signed load read 0x16 from scrubbed
  // zeros after unrelated cases ran) and translator-buffer exhaustion.
  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  uc.mem_map(0, 0x10000, ucMod.PROT_ALL);
  uc.mem_map(0x3f0000, 0x10000, ucMod.PROT_ALL);
  // NOTE: mem_write needs typed arrays — plain JS Arrays silently fail.
  uc.mem_write(0, Buffer.alloc(0x3000));
  uc.mem_write(0x3f0000, Buffer.alloc(0x10000));
  uc.mem_write(0x100, w4(word));
  uc.mem_write(0x1000, Buffer.from(PAT.slice(0, 64)));
  uc.mem_write(0x2000, Buffer.from(PAT.slice(0, 32)));
  uc.mem_write(0x3fff00 - 64, Buffer.from(PAT.slice(0, 64)));
  regs.forEach((v, i) => {
    const id = i < 29 ? ucMod.ARM64_REG_X0 + i : (i === 29 ? ucMod.ARM64_REG_FP : ucMod.ARM64_REG_LR);
    uc.reg_write_i64(id, BigInt(v));
  });
  uc.reg_write_i64(ucMod.ARM64_REG_SP, BigInt(sp));
  uc.reg_write_i64(ucMod.ARM64_REG_NZCV, BigInt(parseInt(nzcv, 2) << 28));
  let fault = null;
  try { uc.emu_start(0x100, 0x104, 0, 1); } catch (e) { fault = String(e).split('\n')[0].slice(0, 40); }
  const r = [];
  for (let i = 0; i < 29; i++) r.push((BigInt(uc.reg_read_i64(ucMod.ARM64_REG_X0 + i)) & 0xffffffffffffffffn).toString(16));
  r.push((BigInt(uc.reg_read_i64(ucMod.ARM64_REG_FP)) & 0xffffffffffffffffn).toString(16));
  r.push((BigInt(uc.reg_read_i64(ucMod.ARM64_REG_LR)) & 0xffffffffffffffffn).toString(16));
  const rsp = (BigInt(uc.reg_read_i64(ucMod.ARM64_REG_SP)) & 0xffffffffffffffffn).toString(16);

  const rpc = Number(uc.arm64_debug(5)).toString(16);
  const rnz = (Number(uc.reg_read_i64(ucMod.ARM64_REG_NZCV)) >>> 28).toString(2).padStart(4, '0');
  const mem = [];
  for (const [base, len] of [[0x1000, 64], [0x2000, 32], [0x3fff00 - 64, 64]]) {
    mem.push(Buffer.from(uc.mem_read(base, len)).toString('hex'));
  }
  try { uc.close(); } catch {}
  return { fault: !!fault, r, sp: rsp, pc: rpc, nzcv: rnz, mem };
}

let pass = 0, fail = 0;
const fails = [];
for (let n = 0; n < names.length; n++) {
  const [g, i, src] = names[n];
  const word = parseInt(words[`${g}_${i}`], 16) >>> 0;
  for (let v = 0; v < VECTORS.length; v++) {
    const V = VECTORS[v];
    const regs = [];
    for (let k = 0; k < 31; k++) regs.push(BigInt(V.r(k)));
    const want = await oracle(word, regs, BigInt(V.sp), V.nzcv);
    const args = ['0x' + word.toString(16),
      ...regs.map((x) => '0x' + x.toString(16)), V.sp, V.nzcv];
    let got;
    try {
      const out = execFileSync(ROOT + '/target/debug/examples/one', args, { maxBuffer: 8 * 1024 * 1024 }).toString();
      const L = Object.fromEntries(out.trim().split('\n').map((l) => {
        const j = l.indexOf(' ');
        return [l.slice(0, j), l.slice(j + 1)];
      }));
      got = {
        fault: L['status'] !== 'ok',
        r: L.regs.split(' '), sp: L.sp.split(' ')[0], pc: L.sp.split(' ')[2],
        nzcv: L.nzcv, mem: L.mem.split(' '),
      };
    } catch (e) { got = { fault: 'CRASH' }; }
    const hx = (s) => { try { return BigInt(s.startsWith('0x') ? s : '0x' + s).toString(16); } catch { return s; } };
    // fault pc: unicorn reports the faulting insn, pi-cpu pre-increments
    // (same convention as cpu-diff.mjs).
    if (want.fault && got.fault) got.pc = '0x' + ((BigInt('0x' + got.pc) - 4n) & 0xffffffffffffffffn).toString(16);
    // branches single-stepped: a bad target faults on the fork immediately
    // but on pi-cpu only at the next fetch — compare pc, not fault.
    // Indirect branches (ret/blr) to odd targets: the fork reports a
    // rounded pc — compare regs/flags only there.
    if (g === 'branch') { want.fault = false; got.fault = false; }
    if (src.startsWith('ret') || src.startsWith('blr')) { want.pc = got.pc; }
    const norm = (o) => JSON.stringify([o.fault, o.r.map(hx), hx(o.sp), hx(o.pc), o.nzcv, o.mem]);
    if (norm(want) === norm(got)) { pass++; }
    else {
      fail++;
      if (fails.length < 15) {
        const dl = [];
        if (want.fault !== got.fault) dl.push(`fault ${want.fault}/${got.fault}`);
        want.r.forEach((v, i) => { if (hx(v) !== hx(got.r[i])) dl.push(`x${i} ${hx(v)}/${hx(got.r[i])}`); });
        if (hx(want.sp) !== hx(got.sp)) dl.push(`sp ${hx(want.sp)}/${hx(got.sp)}`);
        if (hx(want.pc) !== hx(got.pc)) dl.push(`pc ${hx(want.pc)}/${hx(got.pc)}`);
        if (want.nzcv !== got.nzcv) dl.push(`nz ${want.nzcv}/${got.nzcv}`);
        want.mem.forEach((v, i) => { if (v !== got.mem[i]) dl.push(`mem${i} ${v.slice(0, 24)}.. vs ${got.mem[i].slice(0, 24)}..`); });
        fails.push(`${g} ${src} [V${v}] word=${words[`${g}_${i}`]}\n  DIFF: ${dl.join(' | ')}`);
      }
    }
  }
}
console.log(`${pass} ok, ${fail} FAIL`);
for (const f of fails) console.log(f);
process.exit(fail ? 1 : 0);
