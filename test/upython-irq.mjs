import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { PiSess } from './pi-sess.mjs';

// machine.Pin.irq: real vector entry + GPEDS ack + deferred dispatch.
// Needs ports/bcm2837/build/firmware.elf (see test/upython-repl.mjs
// header) and realIrq delivery (CPU_INTERRUPT_HARD via the local block,
// like the lirq guest).
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

const emu = new PiSess();
await emu.loadFirmware(readFileSync(FW));
for (let i = 0; i < 3000 && !emu.consoleText.includes('>>>'); i++) await emu.runSlice(4096);
async function cmd(s, budget = 6000) {
  const before = emu.consoleText.length;
  for (const ch of s) {
    emu.pushKey(ch.charCodeAt(0));
    for (let k = 0; k < 8; k++) await emu.runSlice(512);
  }
  emu.pushKey(13);
  for (let i = 0; i < budget && (emu.consoleText.slice(before).match(/>>>/g) || []).length < 1; i++) {
    await emu.runSlice(4096);
  }
  return emu.consoleText.slice(before);
}

await cmd('from machine import Pin');
await cmd('hits = []');
await cmd('b = Pin(29, Pin.IN)');
await cmd('b.irq(lambda p: hits.append(1), Pin.IRQ_RISING)');
// Async: no keys typed from here on, only slices.
emu.setButton(true);
for (let i = 0; i < 400; i++) await emu.runSlice(4096);
check('press delivers once', (await cmd('len(hits)')).includes('\r\n1\r\n'));
emu.setButton(false);
for (let i = 0; i < 200; i++) await emu.runSlice(4096);
emu.setButton(true);
for (let i = 0; i < 400; i++) await emu.runSlice(4096);
check('repress delivers again, no stuck level', (await cmd('len(hits)')).includes('\r\n2\r\n'));
await cmd('b.irq(None)');
await cmd('hits = []');
// Level phases need the button released first (the rising phases leave it
// held). Release + settle with IRQs disabled so no stale edge fires.
emu.setButton(false);
for (let i = 0; i < 50; i++) await emu.runSlice(4096);
await cmd('b.irq(lambda p: hits.append(1), Pin.IRQ_LOW_LEVEL)');
for (let i = 0; i < 300; i++) await emu.runSlice(4096);
check('low level fires while released', !/\r\n0\r\n/.test(await cmd('len(hits)')));
await cmd('b.irq(None)');
await cmd('hits = []');
await cmd('b.irq(lambda p: hits.append(1), Pin.IRQ_HIGH_LEVEL)');
for (let i = 0; i < 200; i++) await emu.runSlice(4096);
check('high level silent while released', (await cmd('len(hits)')).includes('\r\n0\r\n'));
emu.setButton(true);
for (let i = 0; i < 300; i++) await emu.runSlice(4096);
check('high level fires while held', !/\r\n0\r\n/.test(await cmd('len(hits)')));
await cmd('b.irq(None)');
await cmd('hits = []');
await cmd('b.irq(lambda p: hits.append(1), Pin.IRQ_FALLING)');
for (let i = 0; i < 200; i++) await emu.runSlice(4096);
check('falling silent while held', (await cmd('len(hits)')).includes('\r\n0\r\n'));
emu.setButton(false);
for (let i = 0; i < 400; i++) await emu.runSlice(4096);
check('falling fires on release', (await cmd('len(hits)')).includes('\r\n1\r\n'));
check('no faults', !emu.lastFault, emu.lastFault ? emu.lastFault.message : '');

if (fail.length) { console.log('UPYTHON-IRQ FAIL:', fail.join(', ')); process.exit(1); }
emu.close();
console.log('upython-irq: PASS');
