import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Pi3Emulator, loadUnicorn } from '../packages/pi3-emu/src/index.js';

// MicroPython bare-metal port (ports/bcm2837): banner + REPL + variables +
// frozen import, all headless. Needs ports/bcm2837/build/firmware.elf —
// build it first:
//
//   export PATH="$HOME/toolchains/arm-gnu-toolchain-13.2.Rel1-x86_64-aarch64-none-elf/bin:$PATH"
//   make -C ports/bcm2837
const __dirname = dirname(fileURLToPath(import.meta.url));
const FW = join(__dirname, '..', 'ports', 'bcm2837', 'build', 'firmware.elf');
const fail = [];
function check(name, cond, extra = '') {
  if (cond) console.log('ok', name);
  else { console.log('FAIL', name, extra); fail.push(name); }
}

if (!existsSync(FW)) {
  console.log('SKIP: no firmware (build ports/bcm2837 first, see header)');
  process.exit(0);
}

const ucMod = await loadUnicorn();
const emu = new Pi3Emulator(ucMod);
await emu.loadFirmware(readFileSync(FW));
for (let i = 0; i < 3000 && !emu.consoleText.includes('>>>'); i++) emu.runSlice(4096);
check('banner + REPL prompt', /MicroPython.*pi3-emu with bcm2837/.test(emu.consoleText));

// Drip-feed: the PL011 RX FIFO holds 16 bytes; pasting long lines at once
// truncates them (the guest then waits on a continuation prompt).
async function cmd(s, budget = 6000) {
  const before = emu.consoleText.length;
  for (const ch of s) {
    emu.pushKey(ch.charCodeAt(0));
    for (let k = 0; k < 8; k++) emu.runSlice(512);
  }
  emu.pushKey(13);
  for (let i = 0; i < budget && (emu.consoleText.slice(before).match(/>>>/g) || []).length < 1; i++) {
    emu.runSlice(4096);
  }
  return emu.consoleText.slice(before);
}

check('arith', (await cmd('1+1')).includes('\r\n2\r\n'));
check('float arith', (await cmd('1.5 + 2.25')).includes('\r\n3.75\r\n'));
check('math module', (await cmd('import math')).includes('>>>'));
check('math.sqrt', (await cmd('math.sqrt(2)')).includes('1.4142135623730951'));
check('store/load', (await cmd('x = 41')).includes('>>>') && (await cmd('x + 1')).includes('\r\n42\r\n'));
check('str/list/builtin', (await cmd('print("hi", [1,2], len("abcd"))')).includes('hi [1, 2] 4'));
check('frozen import', /^import boot\r\n/.test(await cmd('import boot')));
const hello = await cmd('boot.hello()');
check('frozen exec', hello.includes('hello from frozen pi3-emu'), hello.slice(0, 120));
check('no faults', !emu.lastFault, emu.lastFault ? emu.lastFault.message : '');

if (fail.length) { console.log('UPYTHON FAIL:', fail.join(', ')); process.exit(1); }
console.log('upython-repl: PASS');
