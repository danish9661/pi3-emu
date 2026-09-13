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
