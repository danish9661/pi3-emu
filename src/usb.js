// Synopsys DesignWare USB 2.0 OTG controller (DWC2) at 0x3F980000:
// the BCM2837's USB peripheral. Linux's dwc2 driver probes this block
// during boot; we model the core identification and minimal control
// registers so the probe succeeds without timeout.
//
// Real register layout (selected, 256K window):
//   +0x00 GOTGCTL    OTG Control
//   +0x04 GOTGINT    OTG Interrupt
//   +0x08 GAHBCFG    AHB Configuration
//   +0x0C GUSBCFG    USB Configuration
//   +0x10 GRSTCTL    Reset Control
//     bit 0: CSRST (core soft reset, auto-clears)
//     bit 30: AHBIDLE (AHB master idle)
//   +0x14 GINTSTS    Interrupt Status (RO, reads 0 — no interrupts)
//   +0x18 GINTMSK    Interrupt Mask
//   +0x1C GRXSTSP    Receive Status Pop (RO, 0 = empty)
//   +0x24 GNPTXSTS   Non-periodic TX Status
//
//   +0x400 HCFG       Host Configuration
//   +0x408 HFIR       Host Frame Interval
//   +0x410 HFNUM      Host Frame Number
//   +0x440 HPTXSTS    Host Periodic TX Status
//   +0x444 HAINT      Host All Channels Interrupt
//   +0x448 HAINTMSK   Host All Channels Interrupt Mask
//
//   +0xC00 GLPMCFG    Core LPM Configuration
//
//   +0x4000+ HCSPLT  Host Split Control (per-channel)
//   +0x5000+ HCCHAR  Host Channel Characteristics
//
// The driver reads core identification at +0x40 (GSNPSID, the Synopsys
// ID), which must return a plausible value (e.g. 0x4f54280a for DWC2
// rev 4.20a). Everything else is stubbed to zero or self-clearing.

export function createUsb(uc, ucMod, base) {
  const SIZE = 0x1000; // 4K covers the core registers; Linux maps 256K
  const GSNPSID = base + 0x40;  // Synopsys ID register
  const GOTGCTL = base + 0x00;
  const GRSTCTL = base + 0x10;
  const GINTSTS = base + 0x14;
  const HCFG   = base + 0x400;
  const HFIR   = base + 0x408;
  const HFNUM  = base + 0x410;

  const state = {
    enabled: false,
    frameNum: 0,
  };

  function syncOut(uc) {
    // Synopsys DesignWare USB 2.0 OTG ID (rev 4.20a, matching BCM2837)
    writeU32(uc, GSNPSID, 0x4f54280a);
    // GOTGCTL: BSVLD (B-session valid) = 1, so the driver knows a cable
    // is connected.
    writeU32(uc, GOTGCTL, 1 << 19);
    // GRSTCTL: AHBIDLE=1 (core idle, no active transfers)
    writeU32(uc, GRSTCTL, 1 << 30);
    // GINTSTS: read-only zeros (no interrupts pending)
    writeU32(uc, GINTSTS, 0);
    // HFNUM: frame number increments each 1ms frame
    state.frameNum = (state.frameNum + 1) & 0x3fff;
    writeU32(uc, HFNUM, state.frameNum);
  }

  function syncIn(uc) {
    state.enabled = (readU32(uc, HCFG) & 1) !== 0;
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
