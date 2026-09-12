import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Pi3Emulator, loadUnicorn } from '../packages/pi3-emu/src/index.js';

// time/utime (system-timer HAL) + machine.Timer PERIODIC/ONE_SHOT via the
// C0..C3 matches. Needs ports/bcm2837/build/firmware.elf (see
// test/upython-repl.mjs header) and realIrq delivery (like Pin.irq).
// NOTE: list.__setitem__ dunder access does not exist at this ROM level —
// callbacks use def + subscript statements, not one-liner dunders.
const __dirname = dirname(fileURLToPath(import.meta.url));
const FW = join(__dirname, '..', 'ports', 'bcm2837', 'build', 'firmware.elf');
const fail = [];
function check(name, cond, extra = '') {
  if (cond) console.log('ok', name);
  else { console.log('FAIL', name, extra); fail.push(name); }
}

if (!existsSync(FW)) {
  console.log('SKIP: no firmware (build ports/bcm2837 first)');
  process.exit(0);
}

const ucMod = await loadUnicorn();
const emu = new Pi3Emulator(ucMod, { realIrq: true });
await emu.loadFirmware(readFileSync(FW));
for (let i = 0; i < 9000 && !emu.consoleText.includes('>>>'); i++) emu.runSlice(4096);
async function cmd(s, budget = 40000, wait = '>>>') {
  const before = emu.consoleText.length;
  for (const ch of s) {
    emu.pushKey(ch.charCodeAt(0));
    for (let k = 0; k < 8; k++) emu.runSlice(512);
  }
  emu.pushKey(13);
  for (let i = 0; i < budget && !emu.consoleText.slice(before).includes(wait); i++) {
    emu.runSlice(4096);
  }
  return emu.consoleText.slice(before);
}

check('time module', (await cmd('import time, utime')).includes('>>>'));
check('ticks advance',
  (await cmd('a = time.ticks_ms()')).includes('>>>') &&
  (await cmd('time.sleep_ms(150)')).includes('>>>') &&
  (await cmd('time.ticks_diff(time.ticks_ms(), a) >= 140')).includes('True'));
check('ticks_us + utime alias',
  (await cmd('u0 = utime.ticks_us()')).includes('>>>') &&
  (await cmd('utime.sleep_us(20000)')).includes('>>>') &&
  (await cmd('utime.ticks_diff(utime.ticks_us(), u0) >= 19000')).includes('True'));
check('periodic timer',
  (await cmd('from machine import Timer')).includes('>>>') &&
  (await cmd('n = [0]')).includes('>>>') &&
  (await cmd('def cb(t):', 40000, '...')).includes('...') &&
  (await cmd('    n[0] += 1', 40000, '...')).includes('...') &&
  (await cmd('')).includes('>>>') &&
  (await cmd('t = Timer(1, mode=Timer.PERIODIC, period=200, callback=cb)')).includes('>>>'));
for (let i = 0; i < 900; i++) emu.runSlice(4096);
check('periodic fired repeatedly', (await cmd('n[0] >= 2')).includes('True'), await cmd('n[0]'));
check('one-shot fires once',
  (await cmd('m = [0]')).includes('>>>') &&
  (await cmd('def cb1(t):', 40000, '...')).includes('...') &&
  (await cmd('    m[0] += 1', 40000, '...')).includes('...') &&
  (await cmd('')).includes('>>>') &&
  (await cmd('s = Timer(2, mode=Timer.ONE_SHOT, period=200, callback=cb1)')).includes('>>>'));
for (let i = 0; i < 900; i++) emu.runSlice(4096);
check('one-shot count', (await cmd('m[0]')).includes('\r\n1\r\n'));
check('deinit stops',
  (await cmd('t.deinit()')).includes('>>>') &&
  (await cmd('k = n[0]')).includes('>>>'));
for (let i = 0; i < 600; i++) emu.runSlice(4096);
check('stopped stays', (await cmd('n[0] == k')).includes('True'));
check('bad args raise',
  (await cmd('Timer(9)')).includes('ValueError') &&
  (await cmd('Timer(0, period=0, callback=cb)')).includes('ValueError'));
check('no faults', !emu.lastFault, emu.lastFault ? emu.lastFault.message : '');

if (fail.length) { console.log('UPYTHON-TIMER FAIL:', fail.join(', ')); process.exit(1); }
console.log('upython-timer: PASS');
