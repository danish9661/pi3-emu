import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { PiSess } from './pi-sess.mjs';

// machine.UART: PL011 TX/RX + mini-UART TX. Needs
// ports/bcm2837/build/firmware.elf (see test/upython-repl.mjs header).
// RX-from-harness is covered implicitly (every typed line arrives via the
// same uart_getc path); here we check construct/config, TX echo, any()
// idle state, and empty-read None.
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
emu.attachUart1();
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

check('import', (await cmd('from machine import UART')).includes('>>>'));
check('construct', (await cmd('u = UART(0, 115200)')).includes('>>>'));
check('any() idle', (await cmd('u.any()')).includes('\r\n0\r\n'));
check('empty read is silent None', (await cmd('u.read()')) === 'u.read()\r\n>>> ');
await cmd('u.write(bytes([104, 105]))');
check('TX echo on console', emu.consoleText.includes('hi'));
check('mini-UART TX tagged', (await cmd('UART(1).write(bytes([81]))')).includes('[u1] '));
check('no faults', !emu.lastFault, emu.lastFault ? emu.lastFault.message : '');

if (fail.length) { console.log('UPYTHON-UART FAIL:', fail.join(', ')); process.exit(1); }
emu.close();
console.log('upython-uart: PASS');
