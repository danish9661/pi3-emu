// BCM2837 GPIO (0x3F200000) — real register layout with event/IRQ support.
// Host-arbitrated like the other windows: output levels are latched host-
// side from GPSET/GPCLR writes, input pins are host-driven (getBtn returns
// the host input level bitmask). Edge events are detected host-side at
// slice boundaries (and GPEDS writes are hooked for real-time de-assert).
//
// Two IRQ lines, one per 32-pin bank, into the legacy IC bank-2 bits 17/18
// (GPU IRQ 81/82): a line is high iff any GPEDS bit is covered by a matching
// event enable (GPREN/GPFEN/GPHEN/GPLEN/GPAREN/GPAFEN) in that bank.
//
//   +0x00 GPFSEL0..  function selects (guest, no host effect)
//   +0x1C GPSET0     write-1 set   (+0x20 GPSET1)
//   +0x28 GPCLR0     write-1 clear (+0x2C GPCLR1)
//   +0x34 GPLEV0     level read    (+0x38 GPLEV1; host refreshes inputs)
//   +0x40 GPEDS0     event detect, W1C (+0x44 GPEDS1)
//   +0x4C GPREN0     rising-edge enable  (+0x50 GPREN1)
//   +0x58 GPFEN0     falling-edge enable (+0x5C GPFEN1)
//   +0x64 GPHEN0     high-level enable   (+0x68 GPHEN1)
//   +0x70 GPLEN0     low-level enable    (+0x74 GPLEN1)
//   +0x7C GPAREN0    async rising-edge   (+0x80 GPAREN1)
//   +0x88 GPAFEN0    async falling-edge  (+0x8C GPAFEN1)
//   +0x94 GPPUD      pull control (guest, no host effect)

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
    // [enable reg addr (bank 0), enable reg addr (bank 1), mask bit is set
    // for rising edges (rise/high/async-rise) vs falling (fall/low/async-fall)]
    [base + 0x4c, base + 0x50, 1], // GPREN
    [base + 0x58, base + 0x5c, 0], // GPFEN
    [base + 0x64, base + 0x68, 1], // GPHEN
    [base + 0x70, base + 0x74, 0], // GPLEN
    [base + 0x7c, base + 0x80, 1], // GPAREN
    [base + 0x88, base + 0x8c, 0], // GPAFEN
  ];

  const state = { out: 0, ev: [0, 0], hostPrev: 0, btn: 0 };

  const readU32 = (uc, addr) => {
    const b = uc.mem_read(addr, 4);
    return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
  };
  const writeU32 = (uc, addr, v) => {
    uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
  };

  // Combined event-enable mask per bank (rise types | fall types).
  function enMasks() {
    const m = [0, 0];
    for (const [a0, a1] of EV_REGS) {
      m[0] |= readU32(uc, a0);
      m[1] |= readU32(uc, a1);
    }
    return m;
  }

  // Bank IRQ lines: any GPEDS bit covered by an event enable.
  const irqActive = (bank) => {
    const m = enMasks();
    return (state.ev[bank] & m[bank]) !== 0;
  };

  // Re-mirror levels + detected events into the guest-visible cells.
  function syncOut(uc) {
    const host = getBtn();
    state.btn = host;
    writeU32(uc, GPLEV0, (state.out & ~host) | host);
    writeU32(uc, GPLEV1, 0);
    const prev = state.hostPrev;
    if (host !== prev) {
      const rise = host & ~prev;
      const fall = prev & ~host;
      const m = enMasks();
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
    // Real GPSET/GPCLR are write-1 registers: mirror the guest's value back
    // into the window so the write itself latches (reads see the latched
    // level, like the real silicon).
    if (set0) writeU32(uc, GPSET0, set0);
    if (set1) writeU32(uc, GPSET1, set1);
    if (clr0) writeU32(uc, GPCLR0, clr0);
    if (clr1) writeU32(uc, GPCLR1, clr1);
    // GPEDS W1C is handled by the write hook (guest accesses only) — the
    // host's own mirror write here would otherwise self-clear the events.
    if (onIrqChange) onIrqChange();
  }

  // Real-time GPEDS de-assert: the guest's W1C write must drop the IRQ line
  // before the handler erets, or the stale high level re-triggers delivery.
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

  return { state, syncOut, syncIn, irqActive };
}