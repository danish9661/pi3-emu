// BCM2835/2837 legacy interrupt controller (0x3F00B200) — the real 3-bank
// layout (verified against Linux irq-bcm2835.c + bcm283x.dtsi):
//
//   pending / enable / disable:
//     +0x00/+0x18/+0x24  basic   bits 0-7 ARM-specific lines, 8 = any bank-1,
//                                9 = any bank-2, 10-14 bank-1 shortcut mirrors,
//                                15-20 bank-2 shortcut mirrors
//     +0x04/+0x10/+0x1C  bank 1  GPU IRQ 0-31: bits 0-3 system timer C0-C3,
//                                16 DMA0, 29 AUX (mini UART)
//     +0x08/+0x14/+0x20  bank 2  GPU IRQ 32-63: 17 GPIO bank 0, 18 GPIO bank 1,
//                                21 I2C0, 22 SPI0, 25 PL011 (IRQ 57), 30 SDHCI
//                                (IRQ 62)
//
// Pending windows show (line & enabled): on real hardware "only bits which
// are enabled can be seen in the interrupt pending registers". Basic bits
// 8/9 are the OR of the non-shortcut enabled lines of bank 1/2; basic bits
// 10-20 mirror the shortcut lines {7,9,10,18,19} (bank 1) and {21,22,23,24,
// 25,30} (bank 2), which can only be masked/unmasked in their own banks.
//
// The GPU line into the local interrupt block (line()) is the OR of every
// pending-and-enabled legacy bit. getLines() returns the host-tracked device
// lines: { timer: C0-C3 match bits, dma0, pl011, sdhci, gpio0, gpio1, aux }.

import { readU32, writeU32 } from './perf.js';

export function createIc(uc, ucMod, base, getLines) {
  const PENDING_BASIC = base + 0x00;
  const PENDING1 = base + 0x04;
  const PENDING2 = base + 0x08;
  const ENABLE_IRQS1 = base + 0x10;
  const ENABLE_IRQS2 = base + 0x14;
  const ENABLE_BASIC = base + 0x18;
  const DISABLE_IRQS1 = base + 0x1c;
  const DISABLE_IRQS2 = base + 0x20;
  const DISABLE_BASIC = base + 0x24;

  // Bank 1 lines.
  const L_TIMER = 0x0f; // C0..C3 match bits 0..3
  const L_DMA0 = 1 << 16;
  const L_AUX = 1 << 29;
  // Bank 2 lines.
  const L_GPIO0 = 1 << 17;
  const L_GPIO1 = 1 << 18;
  const L_PL011 = 1 << 25;
  const L_SDHCI = 1 << 30;
  // Shortcut mirrors (basic 10..14 <- bank1 {7,9,10,18,19},
  // basic 15..20 <- bank2 {21,22,23,24,25,30}).
  const SC1_MASK = (1 << 7) | (1 << 9) | (1 << 10) | (1 << 18) | (1 << 19);
  const SC2_MASK = (1 << 21) | (1 << 22) | (1 << 23) | (1 << 24) | (1 << 25) | (1 << 30);

  const state = { enabled1: 0, enabled2: 0, enabledBasic: 0 };

  // Host device lines -> raw bank lines.
  function rawLines() {
    const l = getLines();
    let b1 = (l.timer || 0) & L_TIMER;
    if (l.dma0) b1 |= L_DMA0;
    if (l.aux) b1 |= L_AUX;
    let b2 = 0;
    if (l.gpio0) b2 |= L_GPIO0;
    if (l.gpio1) b2 |= L_GPIO1;
    if (l.pl011) b2 |= L_PL011;
    if (l.sdhci) b2 |= L_SDHCI;
    return { b1, b2 };
  }

  // Basic-bank shortcut mirrors for the gated lines.
  function scBits(g1, g2) {
    let b = 0;
    if (g1 & (1 << 7)) b |= 1 << 10;
    if (g1 & (1 << 9)) b |= 1 << 11;
    if (g1 & (1 << 10)) b |= 1 << 12;
    if (g1 & (1 << 18)) b |= 1 << 13;
    if (g1 & (1 << 19)) b |= 1 << 14;
    if (g2 & (1 << 21)) b |= 1 << 15;
    if (g2 & (1 << 22)) b |= 1 << 16;
    if (g2 & (1 << 23)) b |= 1 << 17;
    if (g2 & (1 << 24)) b |= 1 << 18;
    if (g2 & (1 << 25)) b |= 1 << 19;
    if (g2 & (1 << 30)) b |= 1 << 20;
    return b;
  }

  // Gated line map (what the pending windows and the GPU line see).
  function pending() {
    const { b1: r1, b2: r2 } = rawLines();
    const g1 = r1 & state.enabled1;
    const g2 = r2 & state.enabled2;
    let basic = 0;
    if (g1 & ~SC1_MASK) basic |= 1 << 8; // any non-shortcut bank-1 line
    if (g2 & ~SC2_MASK) basic |= 1 << 9; // any non-shortcut bank-2 line
    basic |= scBits(g1, g2);
    return { b1: g1, b2: g2, basic };
  }

  // The GPU line into the local interrupt block (OR of all gated lines).
  function line() {
    const p = pending();
    return (p.b1 | p.b2 | p.basic) !== 0 ? 1 : 0;
  }

  function syncOut(uc) {
    const p = pending();
    writeU32(uc, PENDING_BASIC, p.basic);
    writeU32(uc, PENDING1, p.b1);
    writeU32(uc, PENDING2, p.b2);
  }

  function syncIn(uc) {
    const en1 = readU32(uc, ENABLE_IRQS1);
    const dis1 = readU32(uc, DISABLE_IRQS1);
    if (en1) writeU32(uc, ENABLE_IRQS1, 0);
    if (dis1) writeU32(uc, DISABLE_IRQS1, 0);
    state.enabled1 = (state.enabled1 | en1) & ~dis1;
    const en2 = readU32(uc, ENABLE_IRQS2);
    const dis2 = readU32(uc, DISABLE_IRQS2);
    if (en2) writeU32(uc, ENABLE_IRQS2, 0);
    if (dis2) writeU32(uc, DISABLE_IRQS2, 0);
    state.enabled2 = (state.enabled2 | en2) & ~dis2;
    const enB = readU32(uc, ENABLE_BASIC);
    const disB = readU32(uc, DISABLE_BASIC);
    if (enB) writeU32(uc, ENABLE_BASIC, 0);
    if (disB) writeU32(uc, DISABLE_BASIC, 0);
    state.enabledBasic = (state.enabledBasic | enB) & ~disB;
  }

  return { state, syncOut, syncIn, pending, line };
}