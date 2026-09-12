import { existsSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { PiSess } from './pi-sess.mjs';

// os.mount of the FAT12 SD card (VfsFat): boot auto-mount, manual
// remount, listdir, open/read, seek, mkdir/chdir/stat, create+write,
// append, multi-cluster files, package import, and clean errors with no
// card mounted. Needs ports/bcm2837/build/firmware.elf (see
// test/upython-repl.mjs header) with VFS enabled, and the SDHCI window
// attached. Note: this upstream calls the module `os` (no `uos` alias).
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
emu.attachSdhci();
await emu.loadFirmware(readFileSync(FW));
for (let i = 0; i < 9000 && !emu.consoleText.includes('>>>'); i++) await emu.runSlice(4096);
async function cmd(s, budget = 25000) {
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

check('os module', (await cmd('import os')).includes('>>>') &&
  (await cmd('os.mount')).includes('function'));
await cmd('import sdcard');
check('auto-mounted at boot', (await cmd('os.listdir("/sd")')).includes('HELLO.TXT'));
check('manual remount',
  (await cmd('os.umount("/sd")')).includes('>>>') &&
  (await cmd('os.listdir("/sd")')).includes('OSError') &&
  (await cmd('os.mount(os.VfsFat(sdcard.SDCard()), "/sd")')).includes('>>>') &&
  (await cmd('os.listdir("/sd")')).includes('HELLO.TXT'));
check('vfs read', (await cmd('open("/sd/HELLO.TXT").read()')).includes('hello from the SD card'));
check('seek/tell',
  (await cmd('f = open("/sd/HELLO.TXT")')).includes('>>>') &&
  (await cmd('f.seek(6)')).includes('6') &&
  (await cmd('f.tell()')).includes('6') &&
  (await cmd('f.read(4)')).includes('from') &&
  (await cmd('f.close()')).includes('>>>'));
check('mkdir/chdir/stat',
  (await cmd('os.mkdir("/sd/d")')).includes('>>>') &&
  (await cmd('os.chdir("/sd/d")')).includes('>>>') &&
  (await cmd('os.getcwd()')).includes('/sd/') &&
  (await cmd('os.chdir("/")')).includes('>>>') &&
  (await cmd('os.stat("/sd/HELLO.TXT")')).includes('24') &&
  (await cmd('os.statvfs("/sd")')).includes('512'));
check('vfs write',
  (await cmd('f = open("/sd/NEW.TXT", "w")')).includes('>>>') &&
  (await cmd('f.write("written via VfsFat")')).includes('18') &&
  (await cmd('f.close()')).includes('>>>') &&
  (await cmd('open("/sd/NEW.TXT").read()')).includes('written via VfsFat'));
check('append',
  (await cmd('f = open("/sd/NEW.TXT", "a")')).includes('>>>') &&
  (await cmd('f.write("!")')).includes('1') &&
  (await cmd('f.close()')).includes('>>>') &&
  (await cmd('open("/sd/NEW.TXT").read()')).includes('written via VfsFat!'));
check('multi-cluster file',
  (await cmd('f = open("/sd/BIG.BIN", "w")', 60000)).includes('>>>') &&
  (await cmd('f.write("ABCD" * 800)', 60000)).includes('3200') &&
  (await cmd('f.close()', 60000)).includes('>>>') &&
  (await cmd('d = open("/sd/BIG.BIN").read()', 60000)).includes('>>>') &&
  (await cmd('len(d)')).includes('3200') &&
  (await cmd('d[:4]')).includes('ABCD') &&
  (await cmd('d[-4:]')).includes('ABCD'));
check('package import',
  (await cmd('os.mkdir("/sd/mypkg")')).includes('>>>') &&
  (await cmd('f = open("/sd/mypkg/__init__.py", "w")')).includes('>>>') &&
  (await cmd('f.write("VAL = 7\\n")')).includes('8') &&
  (await cmd('f.close()')).includes('>>>') &&
  (await cmd('import mypkg')).includes('>>>') &&
  (await cmd('mypkg.VAL')).includes('7'));
check('import from sd',
  (await cmd('import sys')).includes('>>>') &&
  (await cmd('sys.path.append("/sd")')).includes('>>>') &&
  (await cmd('f = open("/sd/greet.py", "w")')).includes('>>>') &&
  (await cmd('f.write("def hi():\\n    return 42\\n")')).includes('24') &&
  (await cmd('f.close()')).includes('>>>') &&
  (await cmd('import greet')).includes('>>>') &&
  (await cmd('greet.hi()')).includes('42'));
check('unmounted touch fails clean',
  (await cmd('os.umount("/sd")')).includes('>>>') &&
  (await cmd('open("/sd/HELLO.TXT")')).includes('OSError'));
check('python/vfs coherence write',
  (await cmd('sdcard.write("EXTRA.TXT", b"0123456789" * 100)')).includes('1000'));
check('python/vfs coherence remount',
  (await cmd('os.mount(os.VfsFat(sdcard.SDCard()), "/sd")')).includes('>>>') &&
  (await cmd('os.listdir("/sd")')).includes('EXTRA.TXT') &&
  (await cmd('len(open("/sd/EXTRA.TXT").read())')).includes('1000'));
check('no faults', !emu.lastFault, emu.lastFault ? emu.lastFault.message : '');

if (fail.length) { console.log('UPYTHON-VFS FAIL:', fail.join(', ')); process.exit(1); }
emu.close();
console.log('upython-vfs: PASS');
