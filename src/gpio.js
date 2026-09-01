// BCM2837 GPIO (0x3F200000) — real register layout with event/IRQ support.
// Host-arbitrated like the other windows: output levels are latched host-
// side from GPSET/GPCLR writes, input pins are host-driven (getBtn returns
// the host input level bitmask). Edge events are detected host-side at
// slice boundaries (and GPEDS writes are hooked for real-time de-assert).
//
// Performance: enable register caching — the 12-register enMasks() read
// is cached and only refreshed when the guest writes to an enable register.

import { readU32, writeU32 } from './perf.js';

export function createGpio(uc, ucMod, base, { getBtn, onIrqChange }) {
  const GPSET0 = base + 0x1c;
  const GPSET1 = base + 0x20;
  const GPCLR0 = base + 0x28;
  const GPCLR1 = base + 0x2c;
  const GPLEV0 = base + 0x34;
  const GPLEV1 = base + 0x38;
  const GPEDS0 = base + 0x40;
  const GPEDS1 = base + 0x44;
  const EV_REGS = [
    [base + 0x4c, base + 0x50, 1], // GPREN
    [base + 0x58, base + 0x5c, 0], // GPFEN
    [base + 0x64, base + 0x68, 1], // GPHEN
    [base + 0x70, base + 0x74, 0], // GPLEN
    [base + 0x7c, base + 0x80, 1], // GPAREN
    [base + 0x88, base + 0x8c, 0], // GPAFEN
  ];

  const state = { out: 0, ev: [0, 0], hostPrev: 0, btn: 0 };

  // Cached enable masks: only refreshed when guest writes to enable regs.
  let enMaskCache = [0, 0];
  let enMaskDirty = true;

  function refreshEnMasks() {
    const m = [0, 0];
    for (const [a0, a1] of EV_REGS) {
      m[0] |= readU32(uc, a0);
      m[1] |= readU32(uc, a1);
    }
    enMaskCache = m;
    enMaskDirty = false;
  }

  function getEnMasks() {
    if (enMaskDirty) refreshEnMasks();
    return enMaskCache;
  }

  // Bank IRQ lines: any GPEDS bit covered by an event enable.
  const irqActive = (bank) => {
    const m = getEnMasks();
    return (state.ev[bank] & m[bank]) !== 0;
  };

  function syncOut(uc) {
    const host = getBtn();
    state.btn = host;
    writeU32(uc, GPLEV0, (state.out & ~host) | host);
    writeU32(uc, GPLEV1, 0);
    const prev = state.hostPrev;
    if (host !== prev) {
      const rise = host & ~prev;
      const fall = prev & ~host;
      const m = getEnMasks();
      for (let b = 0; b < 2; b++) {
        const r = b === 0 ? rise : 0;
        const f = b === 0 ? fall : 0;
        if (r) state.ev[0] |= r & m[0];
        if (f) state.ev[0] |= f & m[0];
      }
      state.hostPrev = host;
    }
    writeU32(uc, GPEDS0, state.ev[0]);
    writeU32(uc, GPEDS1, state.ev[1]);
  }

  function syncIn(uc) {
    const set0 = readU32(uc, GPSET0);
    const set1 = readU32(uc, GPSET1);
    const clr0 = readU32(uc, GPCLR0);
    const clr1 = readU32(uc, GPCLR1);
    state.out = (state.out | set0) & ~clr0;
    if (set0) writeU32(uc, GPSET0, set0);
    if (set1) writeU32(uc, GPSET1, set1);
    if (clr0) writeU32(uc, GPCLR0, clr0);
    if (clr1) writeU32(uc, GPCLR1, clr1);
    if (onIrqChange) onIrqChange();
  }

  // GPEDS W1C de-assert hook (real-time)
  uc.hook_add(
    ucMod.HOOK_MEM_WRITE,
    (u, access, addr, size, value) => {
      const a = Number(addr);
      const v = Number(value);
      if (a === GPEDS0) {
        state.ev[0] &= ~v;
        writeU32(uc, GPEDS0, state.ev[0]);
      } else if (a === GPEDS1) {
        state.ev[1] &= ~v;
        writeU32(uc, GPEDS1, state.ev[1]);
      }
      if (onIrqChange) onIrqChange();
    },
    null,
    GPEDS0,
    GPEDS1 + 3
  );

  // Invalidate enable mask cache when guest writes to enable registers
  uc.hook_add(
    ucMod.HOOK_MEM_WRITE,
    () => { enMaskDirty = true; },
    null,
    base + 0x4c,  // GPREN0 start
    base + 0x8c + 3 // GPAFEN1 end
  );

  return { state, syncOut, syncIn, irqActive };
}
