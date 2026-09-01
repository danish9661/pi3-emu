// BCM2835 Clock Manager (0x3F100000): primarily used for PWM/I2S/SPI
// clock sources. We model the GPCLK0/1/2 control and divider registers
// plus the CM_PWMCLK control that drives the PWM/I2S peripheral clocks.
//
// Real register layout (selected):
//   +0x00 CM_GPFSEL0   GPU function select (not modelled — GPIO owns this)
//   +0x04 CM_GPFSEL1
//   +0x08 CM_GPFSEL2
//   +0x10 CM_GPSET0
//   +0x14 CM_GPSET1
//   +0x1C CM_GPLEV0
//
//   +0xA0 CM_GPCLK0    GPCLK0 control   (src=oscillator, divi/divf)
//   +0xA4 CM_GPCLK1    GPCLK1 control
//   +0xA8 CM_GPCLK2    GPCLK2 control
//
//   +0xA0 CM_PWMCLK    PWM clock control (same offset family)
//     bit 4: MASH (1 = integer division, 0 = fractional)
//     bits 9-4: CLKSRC (0=oscillator, 1=plld, 5=pllh)
//     bit 11: FLIP (gate the output)
//     bit 15: BUSY (clock is running — host reads 1 when enabled)
//     bit 16: KILL (stop the clock)
//     bit 20: ENAB (enable the clock — guest sets this, host mirrors BUSY)
//     bits 23-12: DIVI (integer divider)
//     bits 11-0: DIVF (fractional divider, not modelled precisely)

export function createClockMgr(uc, ucMod, base) {
  const CM_PWMCLK = base + 0xa0;
  const CM_GPCLK0 = base + 0xa0; // alias: same offset for PWM clock
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
  };

  function syncOut(uc) {
    // When ENAB is set, BUSY follows; when KILL is set, both clear.
    for (const [name, addr] of [['pwmClk', CM_PWMCLK], ['gpclk0', CM_GPCLK0], ['gpclk1', CM_GPCLK1], ['gpclk2', CM_GPCLK2]]) {
      let v = state[name];
      if (v & ENAB_BIT) v |= BUSY_BIT;
      else v &= ~BUSY_BIT;
      if (v & KILL_BIT) { v = 0; state[name] = 0; }
      writeU32(uc, addr, v);
    }
  }

  function syncIn(uc) {
    state.pwmClk = readU32(uc, CM_PWMCLK);
    state.gpclk0 = readU32(uc, CM_GPCLK0);
    state.gpclk1 = readU32(uc, CM_GPCLK1);
    state.gpclk2 = readU32(uc, CM_GPCLK2);
  }

  return { state, syncOut, syncIn };
}

function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}
