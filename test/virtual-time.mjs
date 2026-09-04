import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Pi3Emulator, loadUnicorn, TMR_CLO, TMR_DONE } from '../packages/pi3-emu/src/index.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const fail = [];
function check(name, cond, extra = '') {
  if (cond) console.log('ok', name);
  else { console.log('FAIL', name, extra); fail.push(name); }
}

const ucMod = await loadUnicorn();
const FW = readFileSync(join(__dirname, '..', 'public', 'programs', 'clock.elf'));

async function runOnce() {
  const emu = new Pi3Emulator(ucMod, { virtualTime: true });
  const t0 = Date.now();
  await emu.loadFirmware(FW);
  emu.runUntilDone(() => emu.readU32(TMR_DONE) !== 0, 60000);
  return { wallMs: Date.now() - t0, clo: emu.readU32(TMR_CLO), out: emu.consoleText };
}

const a = await runOnce();
check('virtual clock guest reaches DONE', a.clo > 0, 'clo=' + a.clo);
check('1 s sleep elapses in emulated time (DONE parks right after arming C1)',
  a.clo >= 1000000 && a.clo < 2000000, 'clo=' + a.clo);
check('no wall-clock second slept', a.wallMs < 30000, 'wallMs=' + a.wallMs);
const b = await runOnce();
check('deterministic: identical CLO across runs', a.clo === b.clo, `${a.clo} vs ${b.clo}`);

// Wall-clock mode still works (guest actually sleeps ~1 s).
{
  const emu = new Pi3Emulator(ucMod);
  await emu.loadFirmware(FW);
  const t0 = Date.now();
  emu.runUntilDone(() => emu.readU32(TMR_DONE) !== 0, 60000);
  const wallMs = Date.now() - t0;
  check('wall mode sleeps for real', wallMs >= 900 && emu.readU32(TMR_CLO) >= 1000000,
    `wallMs=${wallMs} clo=${emu.readU32(TMR_CLO)}`);
}

if (fail.length) { console.log('VIRTUAL-TIME FAIL:', fail.join(', ')); process.exit(1); }
console.log('virtual-time: PASS');
