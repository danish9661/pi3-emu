// BCM2837 UART0 — the PL011. This replaces the original "console slots"
// window with the real register model: the guest configures IBRD/FBRD/
// LCRH/CR like a real driver, polls FR for TX/RX flow control, reads and
// writes DR, and RXINTR (IMSC bit 4) / TXINTR (IMSC bit 5) drive the
// interrupt controller's IRQ 57 line (bank 2, bit 25).
//
// Model notes:
//   - TX: a DR write is captured by a HOOK_MEM_WRITE and emitted at write
//     time (same trick as the mini UART), so console chars keep the
//     guest's exact write order. TX is gated on CR.UARTEN (read straight
//     from the window cell, which the guest itself wrote).
//   - RX: the host pushes key bytes into a small FIFO (push()); the head
//     byte is pre-loaded into the DR cell before every slice. RX delivery
//     is split across the two read hooks (see below): this unicorn build
//     runs a read hook *before* the CPU latches the read value, so a hook
//     must never rewrite the register being read — writing the DR cell
//     during a DR read would hand the guest the *next* byte (or 0).
//   - Interrupts: RIS.RXINTR = FIFO non-empty, RIS.TXINTR = 1 while the TX
//     FIFO has room (always — it never fills in this model); MIS = RIS &
//     IMSC; the IC line derives from irqActive(). The TX FIFO has no flow
//     control, so a guest that arms TXIM and never clears it would storm —
//     that is exactly what the uart0 guest demonstrates (arm TXIM, get the
//     IRQ, de-arm it in the handler). Line changes assert/de-assert at
//     slice boundaries, plus a real-time de-assert when a DR read drains
//     the last RX byte (onIrqChange).

import { readU32, writeU32 } from './perf.js';

export function createUart0(uc, ucMod, base, emit, onIrqChange) {
  const DR = base + 0x00;
  const FR = base + 0x18;
  const ICR = base + 0x44;
  const RIS = base + 0x3c;
  const MIS = base + 0x40;

  const FR_RXFE = 1 << 4;
  const FR_RXFF = 1 << 6;
  const FR_TXFE = 1 << 7;
  const RXIM = 1 << 4; // IMSC/RIS/MIS bit 4 = RX interrupt
  const TXIM = 1 << 5; // IMSC/RIS/MIS bit 5 = TX interrupt

  const state = { cr: 0, lcrh: 0, ibrd: 0, fbrd: 0, imsc: 0, rx: [], cap: 16 };

  // The guest has enabled the UART? Read the CR window cell, which holds
  // the guest's own write (host mem_read does not trigger hooks).
  const txEnabled = () => (readU32(uc, base + 0x30) & 1) !== 0;

  // Re-mirror the derived state into the guest-visible cells: the DR cell
  // carries the next RX byte (the byte the guest is about to read), FR
  // shows TX ready + RX empty, RIS/MIS show the raw/masked interrupt
  // status (RIS.TXINTR is always set — the TX FIFO never fills).
  // withDr=false refreshes only FR/RIS/MIS — for the DR read hook, which
  // must not rewrite the DR cell while the guest is reading it.
  const refresh = (withDr = true) => {
    const head = state.rx.length > 0 ? state.rx[0] : 0;
    if (withDr) uc.mem_write(DR, [head]);
    let fr = FR_TXFE; // TX never full: TXFF = 0, TXFE = 1
    if (state.rx.length === 0) fr |= FR_RXFE;
    if (state.rx.length >= state.cap) fr |= FR_RXFF;
    writeU32(uc, FR, fr);
    const raw = (state.rx.length > 0 ? RXIM : 0) | TXIM;
    writeU32(uc, RIS, raw);
    writeU32(uc, MIS, raw & state.imsc);
  };

  uc.hook_add(
    ucMod.HOOK_MEM_WRITE,
    (u, access, addr, size, value) => {
      const b = Number(value) & 0xff;
      // No CR.UARTEN gate here: Linux's earlycon=pl011 writes DR blindly
      // without ever configuring CR, and real firmware leaves the UART
      // enabled — so emit any non-zero byte. The guests configure CR
      // properly anyway; the IRQ path (irqActive) keeps its own gating.
      if (b !== 0) emit(b);
    },
    null,
    DR,
    DR + 3
  );
  // RX delivery (this unicorn build's read hooks fire *before* the CPU
  // latches the read, so a hook may never rewrite the register being
  // read — the guest would see the post-hook value). The guest's RX flow
  // always polls FR before reading DR, so the two reads split the work:
  //   - FR read hook: re-mirrors the DR cell with the current FIFO head
  //     (DR is not being read right now, so writing it is safe).
  //   - DR read hook: pops the FIFO and refreshes FR/RIS/MIS — but never
  //     touches the DR cell (the byte the guest is about to latch stays
  //     put). The next FR poll sees the post-pop FR, so multi-key bursts
  //     pop exactly one byte per DR read, in order.
  uc.hook_add(
    ucMod.HOOK_MEM_READ,
    (u, access, addr, size, value) => {
      refresh();
    },
    null,
    FR,
    FR + 3
  );
  uc.hook_add(
    ucMod.HOOK_MEM_READ,
    (u, access, addr, size, value) => {
      // The byte the guest just read is gone. Never rewrite the DR cell
      // here: the CPU latches this read *after* the hook, so the cell
      // must still hold the byte that was read.
      if (state.rx.length > 0) state.rx.shift();
      refresh(false);
      // Real-time RXINTR de-assert: draining the last byte drops the line
      // before the handler erets, so a stale high level cannot re-trigger
      // delivery (the slice-boundary re-sync would be too late).
      if (state.rx.length === 0 && onIrqChange) onIrqChange();
    },
    null,
    DR,
    DR + 3
  );

  // Latch the guest's register writes out of the window after a slice.
  const syncIn = () => {
    const cr = readU32(uc, base + 0x30);
    if (cr !== state.cr) state.cr = cr;
    const lcrh = readU32(uc, base + 0x2c);
    if (lcrh !== state.lcrh) state.lcrh = lcrh;
    const ibrd = readU32(uc, base + 0x24);
    if (ibrd !== state.ibrd) state.ibrd = ibrd;
    const fbrd = readU32(uc, base + 0x28);
    if (fbrd !== state.fbrd) state.fbrd = fbrd;
    const imsc = readU32(uc, base + 0x38);
    if (imsc !== state.imsc) state.imsc = imsc;
    // ICR is write-1-to-clear: absorb it so reads see 0.
    if (readU32(uc, ICR) !== 0) writeU32(uc, ICR, 0);
    if (onIrqChange) onIrqChange();
  };

  // Host key input: queued only while the guest has enabled RX.
  const push = (b) => {
    if ((state.cr & (1 | (1 << 9))) !== 0 && state.rx.length < state.cap) {
      state.rx.push(b & 0xff);
    }
  };

  // The IC's UART line (IRQ 57, bank-2 bit 25): RXINTR/TXINTR masked by
  // IMSC, gated on the UART being enabled.
  const irqActive = () =>
    ((state.rx.length > 0 && (state.imsc & RXIM) !== 0) || (state.imsc & TXIM) !== 0) &&
    (state.cr & (1 | (1 << 9))) !== 0;

  return { state, syncOut: refresh, syncIn, push, irqActive };
}