import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Pi3Emulator, loadUnicorn } from '../packages/pi3-emu/src/index.js';

// Frozen sdcard.py on the emulated FAT12 card: auto-mount banner at boot,
// listdir, exact HELLO.TXT payload. Needs
// ports/bcm2837/build/firmware.elf (see test/upython-repl.mjs header) and
// the SDHCI window attached (the card lives there, not in the base map).
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
emu.attachSdhci();
await emu.loadFirmware(readFileSync(FW));
for (let i = 0; i < 6000 && !emu.consoleText.includes('>>>'); i++) emu.runSlice(4096);
async function cmd(s, budget = 25000) {
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

check('auto-run banner', emu.consoleText.includes('boot: pi3-emu ready'),
  emu.consoleText.slice(0, 160));
check('ls', (await cmd('import sdcard')).includes('>>>') &&
  (await cmd('sdcard.ls()')).includes("['HELLO.TXT']"));
check('read payload', (await cmd('sdcard.read("HELLO.TXT")')).includes('hello from the SD card'));
check('create multi-cluster',
  (await cmd('sdcard.write("EXTRA.TXT", b"0123456789" * 100)')).includes('1000') &&
  (await cmd('sdcard.ls()')).includes('EXTRA.TXT') &&
  (await cmd('len(sdcard.read("EXTRA.TXT"))')).includes('1000') &&
  (await cmd('sdcard.read("EXTRA.TXT")[:10]')).includes('0123456789'));
check('shrink frees tail',
  (await cmd('sdcard.write("EXTRA.TXT", b"tiny")')).includes('4') &&
  (await cmd('sdcard.read("EXTRA.TXT")')).includes('tiny') &&
  (await cmd('sdcard.read("HELLO.TXT")')).includes('hello from the SD card'));
check('write round-trip',
  (await cmd('sdcard.write("HELLO.TXT", b"pi3-emu wrote this")')).includes('>>>') &&
  (await cmd('sdcard.read("HELLO.TXT")')).includes('pi3-emu wrote this'));
check('raw block round-trip',
  (await cmd('c = sdcard.SDCard()')).includes('>>>') &&
  (await cmd('b = bytearray(512)')).includes('>>>') &&
  (await cmd('c.readblocks(4, b)')).includes('>>>') &&
  (await cmd('b[0]')).includes('112') && // 'p' of the payload above
  (await cmd('b[0] = 80')).includes('>>>') && // 'P': proves the block write stuck
  (await cmd('c.writeblocks(4, b)')).includes('>>>') &&
  (await cmd('sdcard.read("HELLO.TXT")')).includes('Pi3-emu wrote this'));
check('no faults', !emu.lastFault, emu.lastFault ? emu.lastFault.message : '');

if (fail.length) { console.log('UPYTHON-SD FAIL:', fail.join(', ')); process.exit(1); }
console.log('upython-sd: PASS');
