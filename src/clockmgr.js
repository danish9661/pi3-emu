// BCM2835 Clock Manager (0x3F100000): primarily used for PWM/I2S/SPI
// clock sources. We model the GPCLK0/1/2 control and divider registers
// plus the CM_PWMCLK control that drives the PWM/I2S peripheral clocks.
//
// Performance: dirty-flag gated — syncOut/syncIn only run when the guest
// writes to the clock manager window (write hook sets touched).

import { readU32, writeU32 } from './perf.js';

export function createClockMgr(uc, ucMod, base) {
  const CM_PWMCLK = base + 0xa0;
  const CM_GPCLK0 = base + 0xa0;
  const CM_GPCLK1 = base + 0xa4;
  const CM_GPCLK2 = base + 0xa8;

  const BUSY_BIT = 1 << 15;
  const ENAB_BIT = 1 << 20;
  const KILL_BIT = 1 << 16;

  const state = {
    pwmClk: 0,
    gpclk0: 0,
    gpclk1: 0,
    gpclk2: 0,
    touched: false,
    inited: false,
  };

  uc.hook_add(ucMod.HOOK_MEM_WRITE, () => { state.touched = true; }, null, base, base + 0xFFF);

  function syncOut(uc) {
    if (!state.touched && state.inited) return;
    for (const [name, addr] of [['pwmClk', CM_PWMCLK], ['gpclk0', CM_GPCLK0], ['gpclk1', CM_GPCLK1], ['gpclk2', CM_GPCLK2]]) {
      let v = state[name];
      if (v & ENAB_BIT) v |= BUSY_BIT;
      else v &= ~BUSY_BIT;
      if (v & KILL_BIT) { v = 0; state[name] = 0; }
      writeU32(uc, addr, v);
    }
    state.inited = true;
    state.touched = false;
  }

  function syncIn(uc) {
    if (!state.touched) return;
    state.pwmClk = readU32(uc, CM_PWMCLK);
    state.gpclk0 = readU32(uc, CM_GPCLK0);
    state.gpclk1 = readU32(uc, CM_GPCLK1);
    state.gpclk2 = readU32(uc, CM_GPCLK2);
  }

  return { state, syncOut, syncIn };
}
