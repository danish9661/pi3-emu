import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { PiSess } from './pi-sess.mjs';

// machine.I2C/SPI on the emulated buses: sensor reads + JEDEC ID against
// the built-in slaves. Needs ports/bcm2837/build/firmware.elf (see
// test/upython-repl.mjs header).
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
emu.attachI2c(0x3f804000);
emu.attachSpi(0x3f204000);
await emu.loadFirmware(readFileSync(FW));
for (let i = 0; i < 3000 && !emu.consoleText.includes('>>>'); i++) await emu.runSlice(4096);
// Drip-feed: the PL011 RX FIFO holds 16 bytes. Build byte strings with
// bytes([...]) — backslash escapes do not survive the REPL path cleanly.
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

check('import', (await cmd('from machine import I2C, SPI')).includes('>>>'));
check('WHO_AM_I', (await cmd('I2C(1).readfrom_mem(104, 0, 1)')).includes("b'h'")); // 0x68 prints ASCII
check('TEMP', (await cmd('I2C(1).readfrom_mem(104, 16, 2)')).includes("\\x1a\\x00"));
check('COUNTER reads 1,2,3', (await cmd('[I2C(1).readfrom_mem(104, 32, 1) for _ in range(3)]')).includes("[b'\\x01', b'\\x02', b'\\x03']"));
check('scan finds sensor', (await cmd('104 in I2C(1).scan()')).includes('True'));
check('writeto ACKs', (await cmd('I2C(1).writeto(104, bytes([16]))')).includes('1'));
await cmd('r = bytearray(4)');
check('JEDEC transaction', (await cmd('SPI(0).write_readinto(bytes([159,0,0,0]), r)')).includes('>>>'));
check('JEDEC ID', (await cmd('list(r)')).includes('[0, 239, 64, 24]'));
check('no faults', !emu.lastFault, emu.lastFault ? emu.lastFault.message : '');

if (fail.length) { console.log('UPYTHON-I2CSPI FAIL:', fail.join(', ')); process.exit(1); }
emu.close();
console.log('upython-i2cspi: PASS');
