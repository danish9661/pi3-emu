import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Pi3Emulator, loadUnicorn } from '../packages/pi3-emu/src/index.js';

// machine.Pin on real emulated registers: FSEL latches, GPSET/GPCLR drive
// levels, GPLEV mirrors them, button input reads back. Needs
// ports/bcm2837/build/firmware.elf (see test/upython-repl.mjs header).
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
const emu = new Pi3Emulator(ucMod);
await emu.loadFirmware(readFileSync(FW));
for (let i = 0; i < 3000 && !emu.consoleText.includes('>>>'); i++) emu.runSlice(4096);
const R = (a) => emu.readU32(a);
// Drip-feed: the PL011 RX FIFO holds 16 bytes.
async function cmd(s, budget = 4000) {
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
// GPLEV mirrors the output latch at slice boundaries: settle before assert.
function settle() {
  emu.runSlice(512);
  emu.runSlice(512);
}

check('import machine', (await cmd('from machine import Pin')).includes('>>>'));
await cmd('led = Pin(21, Pin.OUT)');
check('FSEL output latched', ((R(0x3F200008) >>> 3) & 7) === 1);
await cmd('led.on()');
settle();
check('LED on drives GPLEV21', ((R(0x3F200034) >>> 21) & 1) === 1);
check('value() reads back True', (await cmd('led.value()')).includes('True'));
await cmd('led.off()');
settle();
check('LED off clears GPLEV21', ((R(0x3F200034) >>> 21) & 1) === 0);
check('value() reads back False', (await cmd('led.value()')).includes('False'));
emu.setButton(true);
check('button press reads True', (await cmd('Pin(29, Pin.IN).value()')).includes('True'));
emu.setButton(false);
check('button release reads False', (await cmd('Pin(29, Pin.IN).value()')).includes('False'));
check('no faults', !emu.lastFault, emu.lastFault ? emu.lastFault.message : '');

if (fail.length) { console.log('UPYTHON-MACHINE FAIL:', fail.join(', ')); process.exit(1); }
console.log('upython-machine: PASS');
