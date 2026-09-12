import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Pi3Emulator, loadUnicorn } from '../packages/pi3-emu/src/index.js';

// Differential test: unicorn.js vs pi-cpu (cpu/examples/run) on the same
// guest ELF + instruction budget. Compares console bytes, X0-X30, SP, PC,
// fault presence. Usage: node test/cpu-diff.mjs [prog] [budget] [slice] [vt_ips] [press1] [release1] [press2] [keybyte] [keyat] [realirq]
// Timer guests (clock) need virtual time on BOTH sides with an exactly
// divisible budget (no partial tail: the host advances time with float
// math, so tails diverge): slice 4096 + vt_ips 262144 advances exactly
// 15625 us/chunk on both sides (integer-exact, no float near integers).
// Button guests (gpio) take an insn-count press schedule applied at slice
// boundaries on both sides (facade setButton + run press args).
const __dirname = dirname(fileURLToPath(import.meta.url));
const prog = process.argv[2] || 'sum';
const budget = Number(process.argv[3] || '200000');
const slice = Number(process.argv[4] || '4096');
const vtIps = Number(process.argv[5] || '0');
const press1 = Number(process.argv[6] || '0');
const release1 = Number(process.argv[7] || '0');
const press2 = Number(process.argv[8] || '0');
const keybyte = Number(process.argv[9] || '0');
const keyat = Number(process.argv[10] || '0');
// realirq=1: native CPU_INTERRUPT_HARD delivery (lirq-style guests with
// native eret); default 0 is host-assisted IRQ_RET delivery.
const realIrq = Number(process.argv[11] || '0') !== 0;
let keyDone = false;

const ucMod = await (async () => {
  // lirq needs the stock single-arch core (public/unicorn.js): the vendored
  // full-fork build's arch-timer gt path is dead (compare never fires —
  // see M38), while stock delivers CNTPNS fine (lirq-probe precedent).
  if (prog === 'lirq') {
    const { createRequire } = await import('node:module');
    const require = createRequire(import.meta.url);
    return require(join(__dirname, '..', 'public', 'unicorn.js'))();
  }
  return loadUnicorn();
})();
const emu = new Pi3Emulator(ucMod, {
  ...(vtIps ? { virtualTime: { ips: vtIps } } : {}),
  ...(realIrq ? { realIrq: true } : {}),
});
await emu.loadFirmware(readFileSync(join(__dirname, '..', 'public', 'programs', `${prog}.elf`)));
const t0 = Date.now();
let steps = 0;
while (emu.stats.insns < budget && !emu.lastError) {
  if (press1 && emu.stats.insns >= press1) emu.setButton(true);
  if (release1 && emu.stats.insns >= release1) emu.setButton(false);
  if (press2 && emu.stats.insns >= press2) emu.setButton(true);
  if (keybyte && keyat && !keyDone && emu.stats.insns >= keyat) {
    emu.pushKey(keybyte);
    keyDone = true;
  }
  emu.runSlice(Math.min(slice, budget - emu.stats.insns));
  steps++;
  if (steps > budget) break;
}
const uMicros = (Date.now() - t0) * 1000;
const U64 = (v) => (BigInt(v) & 0xFFFFFFFFFFFFFFFFn).toString();
const uregs = [];
for (let i = 0; i < 29; i++) {
  try { uregs.push(U64(emu.uc.reg_read_i64(ucMod.ARM64_REG_X0 + i))); }
  catch { uregs.push('?'); }
}
// X29/X30 have irregular IDs (FP=1, LR=2), not X0+29/30.
try { uregs.push(U64(emu.uc.reg_read_i64(ucMod.ARM64_REG_FP))); } catch { uregs.push('?'); }
try { uregs.push(U64(emu.uc.reg_read_i64(ucMod.ARM64_REG_LR))); } catch { uregs.push('?'); }
let usp = '?';
try { usp = U64(emu.uc.reg_read_i64(ucMod.ARM64_REG_SP)); } catch {}
let upc = 0;
try { upc = Number(emu.uc.arm64_debug(5)); } catch {}
const u = {
  console: emu.consoleText, x: uregs, sp: usp, pc: upc,
  insns: emu.stats.insns, fault: emu.lastError ? String(emu.lastError).slice(0, 60) : null,
  micros: uMicros,
};

const out = execFileSync(join(__dirname, '..', 'target', 'release', 'examples', 'run'),
  [join(__dirname, '..', 'public', 'programs', `${prog}.elf`), String(budget), String(slice), String(vtIps),
   String(press1), String(release1), String(press2), String(keybyte), String(keyat)],
  { maxBuffer: 64 * 1024 * 1024 }).toString();
const lines = Object.fromEntries(out.trim().split('\n').map((l) => {
  const i = l.indexOf('\t');
  return [l.slice(0, i), l.slice(i + 1)];
}));
const r = {
  console: JSON.parse(`"${lines.console}"`),
  x: lines.regs.split(' '),
  sp: lines.meta.split(' ')[0],
  pc: Number(lines.meta.split(' ')[1]),
  insns: Number(lines.meta.split(' ')[2]),
  fault: lines.meta.split(' ')[3] === 'null' ? null : lines.meta.split(' ')[3],
  micros: Number(lines.meta.split(' ')[4]),
};

let fails = 0;
const check = (name, a, b) => {
  const ok = JSON.stringify(a) === JSON.stringify(b);
  console.log(ok ? 'ok' : 'FAIL', name, ok ? '' : `\n  unicorn: ${JSON.stringify(a).slice(0, 300)}\n  pi-cpu:  ${JSON.stringify(b).slice(0, 300)}`);
  if (!ok) fails++;
};
// Faulting guests: the facade overcounts insns to the slice end on fault
// (stats.insns += n unconditionally), and pi-cpu reports pc past the
// faulting insn (step pre-increments). Require fault-on-both + identical
// console/regs/sp, allow pc-4, skip insns.
const faulted = !!u.fault || r.fault !== null;
check('console', u.console, r.console);
check('x0-x30', u.x, r.x.map(String));
check('sp', String(u.sp), String(r.sp));
if (faulted) {
  check('fault-both', true, !!u.fault && r.fault !== null);
  check('pc-fault', u.pc, r.pc - 4);
} else {
  check('pc', u.pc, r.pc);
  check('insns', u.insns, r.insns);
  check('fault', false, r.fault !== null);
}
console.log(`mips: unicorn ${(u.insns / uMicros).toFixed(2)} | pi-cpu ${(r.insns / r.micros).toFixed(2)}`);
if (fails) process.exit(1);
console.log('cpu-diff: PASS');
