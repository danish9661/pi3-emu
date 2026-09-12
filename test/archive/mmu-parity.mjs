import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

// mmu parity: the two cores complete this guest by irreconcilably
// different paths (unicorn needs probe-style fault-retry around the
// same-slice enable+use race — nested mem_map inside a write hook
// traps the fork — while pi-cpu completes natively via the MMU_CTL
// compat regime), so cpu-diff's exact-state method cannot show PASS.
// Instead this script compares the full guest-visible behavior: the
// probe's console (unicorn) vs a standalone pi-cpu run.
const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');

const probeOut = execFileSync('node', [join(__dirname, 'mmu-probe.mjs')], {
  cwd: ROOT, maxBuffer: 64 * 1024 * 1024,
}).toString();
const want = probeOut.split('\n').filter((l) => l.startsWith('mmu: '));

const runOut = execFileSync(join(ROOT, 'target', 'release', 'examples', 'run'),
  [join(ROOT, 'public', 'programs', 'mmu.elf'), '300000', '64', '0'],
  { maxBuffer: 64 * 1024 * 1024 }).toString();
const L = Object.fromEntries(runOut.trim().split('\n').map((l) => {
  const j = l.indexOf('\t');
  return [l.slice(0, j), l.slice(j + 1)];
}));
const got = JSON.parse(`"${L.console}"`).split('\r\n').filter((l) => l.startsWith('mmu: '));

const same = JSON.stringify(want) === JSON.stringify(got);
console.log('unicorn lines:', want.length, 'pi-cpu lines:', got.length);
if (!same) {
  console.log('WANT:', JSON.stringify(want).slice(0, 400));
  console.log('GOT: ', JSON.stringify(got).slice(0, 400));
  console.log('mmu-parity: FAIL');
  process.exit(1);
}
console.log('mmu-parity: PASS');
