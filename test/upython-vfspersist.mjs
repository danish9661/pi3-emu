import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { PiSess } from './pi-sess.mjs';

// SD card snapshot round-trip: write files, exportCard the sector image,
// fresh emulator + importCard before boot, boot-mounted /sd shows them.
// Needs ports/bcm2837/build/firmware.elf (see test/upython-repl.mjs
// header) and the SDHCI window attached.
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

const FW_BYTES = readFileSync(FW);
async function cmd(emu, s, budget = 40000) {
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

// Session 1: create files, export the image. One writer at a time: the
// boot mount is live, so umount before the raw write (a VFS write-back
// from its stale cache would clobber raw-written sectors), remount for
// the VFS write.
const emu1 = new PiSess();
emu1.attachSdhci();
await emu1.loadFirmware(FW_BYTES);
for (let i = 0; i < 9000 && !emu1.consoleText.includes('>>>'); i++) await emu1.runSlice(4096);
await cmd(emu1, 'import sdcard, os');
await cmd(emu1, 'os.umount("/sd")');
check('setup write', (await cmd(emu1, 'sdcard.write("KEEP.TXT", b"persist me" * 50)')).includes('500'));
await cmd(emu1, 'os.mount(os.VfsFat(sdcard.SDCard()), "/sd")');
check('setup vfs write',
  (await cmd(emu1, 'f = open("/sd/VIAVFS.TXT", "w")')).includes('>>>') &&
  (await cmd(emu1, 'f.write("vfs side")')).includes('8') &&
  (await cmd(emu1, 'f.close()')).includes('>>>'));
const image = await emu1.exportCard();
check('export image', image instanceof Uint8Array && image.length % 512 === 0 && image.length >= 5 * 512,
  image ? String(image.length) : 'null');

// Session 2: fresh emulator, import before boot, files survive.
const emu2 = new PiSess();
emu2.attachSdhci();
check('import image', await emu2.importCard(image) === true);
await emu2.loadFirmware(FW_BYTES);
for (let i = 0; i < 9000 && !emu2.consoleText.includes('>>>'); i++) await emu2.runSlice(4096);
check('boot lists kept files', emu2.consoleText.includes('KEEP.TXT') && emu2.consoleText.includes('VIAVFS.TXT'),
  emu2.consoleText.slice(-120));
check('kept raw file', (await cmd(emu2, 'import sdcard')).includes('>>>') &&
  (await cmd(emu2, 'len(sdcard.read("KEEP.TXT"))')).includes('500'));
check('kept vfs file', (await cmd(emu2, 'open("/sd/VIAVFS.TXT").read()')).includes('vfs side'));
check('no faults', !emu2.lastFault, emu2.lastFault ? emu2.lastFault.message : '');

if (fail.length) { console.log('UPYTHON-VFSPERSIST FAIL:', fail.join(', ')); process.exit(1); }
emu1.close(); emu2.close();
console.log('upython-vfspersist: PASS');
