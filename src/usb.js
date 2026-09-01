// Synopsys DesignWare USB 2.0 OTG controller (DWC2) at 0x3F980000.
//
// Expanded for Linux dwc2 driver probe: more registers (GUSBCFG, GAHBCFG,
// GINTMSK, GRXSTSP, GNPTXSTS, GLPMCFG) so the driver's init sequence
// completes without timeout.
//
// Performance: write-once ID registers + dirty-flag gating.

import { readU32, writeU32 } from './perf.js';

export function createUsb(uc, ucMod, base) {
  const GOTGCTL  = base + 0x00;
  const GOTGINT  = base + 0x04;
  const GAHBCFG  = base + 0x08;
  const GUSBCFG  = base + 0x0C;
  const GRSTCTL  = base + 0x10;
  const GINTSTS  = base + 0x14;
  const GINTMSK  = base + 0x18;
  const GRXSTSP  = base + 0x1C;
  const GNPTXSTS = base + 0x24;
  const GSNPSID  = base + 0x40;
  const HCFG     = base + 0x400;
  const HFIR     = base + 0x408;
  const HFNUM    = base + 0x410;
  const HPTXSTS  = base + 0x440;
  const HAINT    = base + 0x444;
  const HAINTMSK = base + 0x448;
  const GLPMCFG  = base + 0xD00;

  const state = {
    enabled: false,
    frameNum: 0,
    inited: false,
    touched: false,
    gahbcfg: 0,
    gusbcfg: 0,
    gintmsk: 0,
  };

  // Write-once init: ID registers that never change.
  function initRegs(uc) {
    writeU32(uc, GSNPSID, 0x4f54280a);   // DWC2 rev 4.20a
    writeU32(uc, GOTGCTL, 1 << 19);      // BSVLD (cable connected)
    writeU32(uc, GRSTCTL, 1 << 30);      // AHBIDLE
    writeU32(uc, GINTSTS, 0);            // no interrupts pending
    writeU32(uc, GINTMSK, 0);            // all interrupts masked
    writeU32(uc, GRXSTSP, 0);            // empty receive FIFO
    writeU32(uc, GNPTXSTS, 0x0008_0004); // non-periodic TX: 8 spaces, 4 free
    writeU32(uc, HPTXSTS, 0x0008_0004);  // periodic TX: 8 spaces, 4 free
    writeU32(uc, HAINT, 0);              // no channel interrupts
    writeU32(uc, HAINTMSK, 0);           // no channel interrupts masked
    writeU32(uc, GLPMCFG, 0);            // LPM disabled
    state.inited = true;
  }

  function syncOut(uc) {
    if (!state.inited) initRegs(uc);
    // Refresh HFNUM if driver is active
    if (state.enabled || state.touched) {
      state.frameNum = (state.frameNum + 1) & 0x3fff;
      writeU32(uc, HFNUM, state.frameNum);
    }
    state.touched = false;
  }

  function syncIn(uc) {
    const hcfg = readU32(uc, HCFG);
    const wasEnabled = state.enabled;
    state.enabled = (hcfg & 1) !== 0;
    state.gahbcfg = readU32(uc, GAHBCFG);
    state.gusbcfg = readU32(uc, GUSBCFG);
    state.gintmsk = readU32(uc, GINTMSK);
    if (state.enabled !== wasEnabled) state.touched = true;
  }

  // Mark dirty when guest writes any register in our window
  uc.hook_add(
    ucMod.HOOK_MEM_WRITE,
    () => { state.touched = true; },
    null,
    base,
    base + 0x1000 - 1
  );

  return { state, syncOut, syncIn };
}
