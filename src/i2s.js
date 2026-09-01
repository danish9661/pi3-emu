// BCM2835 I2S (PCM) audio interface (0x3F203000): serial audio bus used
// for HDMI audio, codec connections, and Hifiberry-style DACs.
//
// Performance: dirty-flag gated — syncOut/syncIn only run when the guest
// writes to the I2S window or when there's FIFO data to drain.

import { readU32, writeU32 } from './perf.js';

export function createI2s(uc, ucMod, base) {
  const CS_A = base + 0x00;
  const FIFO_A = base + 0x04;
  const MODE_A = base + 0x08;
  const RXC_A = base + 0x0c;
  const TXC_A = base + 0x10;
  const DREQ_A = base + 0x14;
  const INTEN_A = base + 0x18;
  const INTSTC_A = base + 0x1c;

  const EN = 1 << 0;
  const TXON = 1 << 1;
  const RXON = 1 << 2;
  const TXW = 1 << 15;
  const RXR = 1 << 16;
  const TXE = 1 << 26;

  const state = {
    cs: 0,
    mode: 0,
    rxc: 0,
    txc: 0,
    dreq: 0,
    inten: 0,
    txFifo: [],
    rxFifo: [],
    enabled: false,
    touched: false,
    inited: false,
  };

  function syncOut(uc) {
    if (!state.touched && state.inited) return;
    if (!state.enabled) {
      writeU32(uc, CS_A, 0);
      state.inited = true;
      state.touched = false;
      return;
    }
    let cs = state.cs & (EN | TXON | RXON);
    cs |= TXW | TXE;
    if (state.rxFifo.length > 0) cs |= RXR;
    cs &= ~(1 << 9 | 1 << 10);
    writeU32(uc, CS_A, cs);
    if (state.txFifo.length > 0) {
      writeU32(uc, FIFO_A, state.txFifo[0]);
    }
    state.inited = true;
    state.touched = false;
  }

  function syncIn(uc) {
    if (!state.touched) return;
    state.cs = readU32(uc, CS_A);
    state.enabled = (state.cs & EN) !== 0;
    state.mode = readU32(uc, MODE_A);
    state.rxc = readU32(uc, RXC_A);
    state.txc = readU32(uc, TXC_A);
    state.dreq = readU32(uc, DREQ_A);
    state.inten = readU32(uc, INTEN_A);
    const ack = readU32(uc, INTSTC_A);
    if (ack & (1 << 9)) state.cs &= ~(1 << 9);
    if (ack & (1 << 10)) state.cs &= ~(1 << 10);
  }

  function onWrite() { state.touched = true; }

  function onFifoWrite(_uc2, _access, _address, _size, value) {
    state.touched = true;
    if (state.enabled) {
      state.txFifo.push(Number(value) >>> 0);
      if (state.txFifo.length > 64) state.txFifo.shift();
    }
  }

  uc.hook_add(ucMod.HOOK_MEM_WRITE, onFifoWrite, null, FIFO_A, FIFO_A + 4);
  uc.hook_add(ucMod.HOOK_MEM_WRITE, onWrite, null, base, base + 0xFFF);

  return { state, syncOut, syncIn };
}
