import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Pi3Emulator, loadUnicorn, I2C_BASE, TMR_DONE } from '../src/index.js';

const __dirname = dirname(fileURLToPath(import.meta.url));
const fail = [];
function check(name, cond, extra = '') {
  if (cond) console.log('ok', name);
  else { console.log('FAIL', name, extra); fail.push(name); }
}

const ucMod = await loadUnicorn();

// 1. shell: boot, prompt, hi -> HELLO, time command.
{
  const emu = new Pi3Emulator(ucMod);
  await emu.loadFirmware(readFileSync(join(__dirname, '..', 'firmware', 'shell.elf')));
  emu.runUntilIdle();
  check('shell boots to prompt', /> $/.test(emu.consoleText), JSON.stringify(emu.consoleText.slice(-40)));
  emu.sendLine('hi');
  emu.runUntilIdle();
  check('shell hi -> HELLO', emu.consoleText.includes('HELLO'));
  emu.sendLine('time');
  emu.runUntilIdle();
  check('shell time prints counter', /time:? [0-9]+/i.test(emu.consoleText));
}

// 2. i2c guest against the built-in slave: WHO_AM_I/TEMP/COUNTER + DONE park.
{
  const emu = new Pi3Emulator(ucMod);
  const i2c = emu.attachI2c(I2C_BASE);
  await emu.loadFirmware(readFileSync(join(__dirname, '..', 'firmware', 'i2c.elf')));
  emu.runUntilDone(() => i2c.state.done);
  check('i2c guest parks DONE', i2c.state.done === true);
  check('i2c WHO_AM_I', emu.consoleText.includes('0x68'), emu.consoleText.slice(0, 200));
  check('i2c temp', emu.consoleText.includes('26'), emu.consoleText.slice(0, 200));
  check('i2c counter', /counter = 3/.test(emu.consoleText));
}

// 3. Facade surface: button mask, LEDs, stats advance.
{
  const emu = new Pi3Emulator(ucMod);
  await emu.loadFirmware(readFileSync(join(__dirname, '..', 'firmware', 'shell.elf')));
  emu.setButton(true);
  emu.runSlice(512);
  emu.setButton(false);
  check('stats advance', emu.stats.steps > 0 && emu.stats.insns > 0);
  check('ledLevels shape', Array.isArray(emu.ledLevels()) && emu.ledLevels().length === 8);
  check('readU32 timer ticks', emu.readU32(TMR_DONE) === 0);
}

if (fail.length) { console.log('SMOKE FAIL:', fail.join(', ')); process.exit(1); }
console.log('pi3-emu smoke: PASS');
