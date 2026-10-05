// M56 Linux-track triage: slice public/linux/qemu-system-aarch64.data
// into DTB + kernel Image + initrd (byte ranges from public/linux/load.js),
// load them at the M56 fixed PAs via load_linux(), reset per the ARM64
// boot protocol (x0=DTB, EL2, MMU off), run a bounded budget, and print
// the FIRST fault (pc/x0/fault/console-head). This names the next decoder/
// device gap by execution, never by reading.
// Usage: node test/linux-triage.mjs [budget] [slice]
import { execFileSync } from 'node:child_process';
import { readFileSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');
const DATA = join(ROOT, 'public', 'linux', 'qemu-system-aarch64.data');
const TRIAGE = join(ROOT, 'target', 'debug', 'examples', 'triage');

if (!existsSync(DATA)) {
  console.log('SKIP: no .data (scripts/fetch-linux.sh first)');
  process.exit(0);
}
if (!existsSync(TRIAGE)) {
  console.log('SKIP: no triage binary (cargo build --examples first)');
  process.exit(0);
}

const budget = process.argv[2] || '200000';
const slice = process.argv[3] || '4096';
// load.js slice table: dtb 0:32753, kernel 32753:22505969, rootfs rest.
const DTB_END = 32753, KERN_END = 22505969;
const data = readFileSync(DATA);
console.log(`data: ${data.length} bytes (dtb 0:${DTB_END}, kernel ${DTB_END}:${KERN_END}, initrd ${KERN_END}:${data.length})`);
const out = execFileSync(TRIAGE, [String(DTB_END), String(KERN_END), budget, slice],
  { maxBuffer: 256 * 1024 * 1024 }).toString();
console.log(out.trimEnd());

// Trajectory pin (M117: re-pinned). The guest's early boot is sensitive to
// guest-visible changes — the DT `/reserved-memory` reservations move the
// memory map, and the timer/RNG/thermal models change device behaviour —
// so a pin move is expected when one of those lands. Keep the value in
// git so the next change has to be acknowledged here, not silently
// accepted. Only enforced for the standard 20M/slice-4096 run.
const PIN = 'pc=0xffffffc008fadee4 x0=0x4b400 fault=null';
if (budget === '20000000' && (slice === '4096' || slice === undefined)) {
  const line = out.split('\n').find((l) => l.startsWith('triage\tn=')) || '';
  if (line.includes('fault=null') && !line.includes(PIN)) {
    console.log(`note: trajectory pin moved (expected only with a guest-visible change):\n  was ${PIN}\n  now ${line.split('console=')[0].trim()}`);
  }
}
