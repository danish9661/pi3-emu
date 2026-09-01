// BCM2836/2837 ARM-local interrupt controller at 0x40000000 — the per-core
// interrupt block that sits on the ARM side (as opposed to the VideoCore IC
// at 0x3F00B200). Each core has a read-only IRQ source register; bit 8 of
// it is the GPU IRQ, routed to a core by the GPU_ROUTING register (default
// core 0).
//
// Real delivery: the host keeps the per-core source bits, mirrors them into
// the guest window before every slice, and asserts/de-asserts the CPU IRQ
// line (CPU_INTERRUPT_HARD) via uc.arm64_set_irq() — the line is the OR of
// the sources the JS device layer owns. The local block has no per-source
// mask (masking happens in DAIF.I and in the peripheral enables), matching
// the hardware.
//
// Source bits (CORE_IRQ_SRC = base + 0x60 + 4*core):
//   0 CNTPSIRQ, 1 CNTPNSIRQ, 2 CNTHPIRQ, 3 CNTVIRQ   (arch timers)
//   4-7 mailbox 0-3, 8 GPU, 9 PMU, 10 AXI, 11 local timer
//
// The arch-timer lines come from the CPU core itself (uc_arm64_debug sels
// 3, 11-14) and are REPORTED in the source register but never drive the
// host line: the timer path already asserts/de-asserts CPU_INTERRUPT_HARD
// inside the core in real time (gt_recalc on every sysreg write), while a
// host-side line can only change at slice boundaries — a stale asserted
// line would re-trigger delivery after the guest's eret. The host line
// therefore tracks only GPU, PMU, AXI, local-timer and mailbox sources.
// The GPU line aggregates the GPU peripheral lines (system timer C1/C3,
// PL011 RXINTR, GPIO, SDHCI) — the caller's getGpuIrq derives it from the
// legacy IC's (pending & enabled). The local timer and mailbox lines come
// from getLines() and are masked by the guest's CORE_TIMER_CTRL /
// MAILBOX_CTRL bits. Writing CONTROL bit n clears core n's local timer
// (W1C, host-tracked).

export function createLocalInt(uc, ucMod, base, getLines) {
  const CONTROL = base + 0x00;
  const PRESCALER = base + 0x08;
  const GPU_ROUTING = base + 0x0c;
  const PM_ROUTING_SET = base + 0x10;
  const PM_ROUTING_CLR = base + 0x14;
  const CORE_TIMER_CTRL = base + 0x40;
  const MAILBOX_CTRL = base + 0x50;
  const CORE_IRQ_SRC = base + 0x60;
  const CORE_FIQ_SRC = base + 0x70;

  const B_CNTPS = 0;
  const B_CNTPNS = 1;
  const B_CNTHP = 2;
  const B_CNTV = 3;
  const B_MBOX0 = 4;
  const B_GPU = 8;
  const B_PMU = 9;
  const B_AXI = 10;
  const B_LTIMER = 11;

  const state = {
    control: 0,
    prescaler: 0,
    gpuRouting: 0,
    pmRoutingSet: 0,
    pmRoutingClr: 0,
    coreTimer: [0, 0, 0, 0],
    mailbox: [0, 0, 0, 0],
    ltimer: [0, 0, 0, 0],
  };

  const readU32 = (uc, addr) => {
    const b = uc.mem_read(addr, 4);
    return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
  };

  const writeU32 = (uc, addr, v) => {
    uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
  };

  // Cache lines across syncOut/syncIrq to avoid redundant arm64_debug calls.
  let _cachedLines = null;

  // The raw pending bits for one core (before CONTROL-side masking).
  function coreSource(core, cached) {
    const l = cached || getLines();
    let s = 0;
    if (l.cntps) s |= 1 << B_CNTPS;
    if (l.cntpns) s |= 1 << B_CNTPNS;
    if (l.cnthp) s |= 1 << B_CNTHP;
    if (l.cntv) s |= 1 << B_CNTV;
    if (l.gpu && (state.gpuRouting & 3) === core) s |= 1 << B_GPU;
    if (l.pmu) s |= 1 << B_PMU;
    if (l.axi) s |= 1 << B_AXI;
    if (state.coreTimer[core] & 1) s |= 1 << B_LTIMER; // local timer enable
    for (let m = 0; m < 4; m++) {
      if (l.mailbox[m] && (state.mailbox[core] & (1 << m))) s |= 1 << (B_MBOX0 + m);
    }
    return s;
  }

  // The bits the host line tracks (arch-timer bits are owned by the core).
  function lineSource(core, cached) {
    const s = coreSource(core, cached);
    return s & ~((1 << B_CNTPS) | (1 << B_CNTPNS) | (1 << B_CNTHP) | (1 << B_CNTV));
  }

  function syncOut(uc) {
    _cachedLines = getLines();
    for (let i = 0; i < 4; i++) {
      writeU32(uc, CORE_IRQ_SRC + i * 4, coreSource(i, _cachedLines));
      writeU32(uc, CORE_FIQ_SRC + i * 4, 0);
      writeU32(uc, CORE_TIMER_CTRL + i * 4, state.coreTimer[i]);
      writeU32(uc, MAILBOX_CTRL + i * 4, state.mailbox[i]);
    }
    writeU32(uc, CONTROL, state.control);
    writeU32(uc, PRESCALER, state.prescaler);
    writeU32(uc, GPU_ROUTING, state.gpuRouting);
    writeU32(uc, PM_ROUTING_SET, state.pmRoutingSet);
    writeU32(uc, PM_ROUTING_CLR, state.pmRoutingClr);
  }

  function syncIn(uc) {
    const control = readU32(uc, CONTROL);
    for (let i = 0; i < 4; i++) {
      if (control & (1 << i)) state.ltimer[i] = 0; // W1C: clear core timer
    }
    state.control = control & ~0xf; // W1C bits do not read back
    state.prescaler = readU32(uc, PRESCALER);
    state.gpuRouting = readU32(uc, GPU_ROUTING);
    state.pmRoutingSet = readU32(uc, PM_ROUTING_SET);
    state.pmRoutingClr = readU32(uc, PM_ROUTING_CLR);
    for (let i = 0; i < 4; i++) {
      state.coreTimer[i] = readU32(uc, CORE_TIMER_CTRL + i * 4);
      state.mailbox[i] = readU32(uc, MAILBOX_CTRL + i * 4);
    }
  }

  // Recompute the CPU IRQ line after a slice's worth of guest writes: the
  // line is high iff the routed core has any host-owned pending source bit.
  // Only core 0 is emulated as a live CPU (SMP uses separate unicorns
  // without this). Guests must de-assert through the peripheral ack paths,
  // which the host re-checks in real time via write hooks.
  function syncIrq(uc, setIrq) {
    const src = lineSource(0, _cachedLines);
    _cachedLines = null;
    const want = src !== 0 ? 1 : 0;
    if (want !== state.lastLine) {
      state.lastLine = want;
      setIrq(want);
    }
  }

  return { syncOut, syncIn, syncIrq, coreSource, lineSource };
}
