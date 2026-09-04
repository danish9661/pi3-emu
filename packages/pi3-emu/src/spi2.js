// BCM2835 AUX SPI2 (0x3F2150C0): third SPI bus, AUX block.
// Same design as SPI1 (src/spi1.js) but CS at +0xC0, ENABLES bit 2.

import { readU32, writeU32 } from './perf.js';

export function createSpi2(uc, ucMod, base, onBridgeData) {
  const CS = base + 0xC0;
  const FIFO = base + 0xC4;
  const CLK = base + 0xC8;
  const DLEN = base + 0xCC;
  const ENABLES = base + 0x04;

  const TA = 1 << 24;
  const CLEAR_BIT = 1 << 16;
  const S_DONE = 1 << 7;
  const S_TXD = 1 << 1;
  const S_RXD = 1 << 2;

  const state = {
    enabled: false,
    tx: [],
    rx: [],
    cmd: 0,
    ta: false,
    sDone: false,
    guard: false,
    clk: 0,
    dlen: 0,
    touched: false,
    inited: false,
  };

  function slaveResponse(cmd, idx) {
    if (idx === 0) return 0x00;
    if (cmd === 0x9f) {
      if (idx === 1) return 0xef;
      if (idx === 2) return 0x40;
      if (idx === 3) return 0x18;
    }
    return 0xff;
  }

  function pushTx(size, value) {
    for (let i = 0; i < size; i++) {
      const b = (value >>> (8 * i)) & 0xff;
      state.tx.push(b);
      state.rx.push(slaveResponse(state.cmd, state.tx.length - 1));
    }
  }

  function process() {
    if (state.tx.length > 0) {
      if (onBridgeData) {
        state.sDone = false;
        onBridgeData({ type: 'spi2-tx', bytes: state.tx.slice() });
      } else {
        state.sDone = true;
      }
    }
  }

  function onFifoWrite(_uc, _access, address, size, value) {
    state.touched = true;
    if (state.guard) return;
    if (Number(address) < FIFO || Number(address) > FIFO + 3) return;
    const v = Number(value) >>> 0;
    if (state.tx.length === 0) state.cmd = v & 0xff;
    pushTx(size, v);
  }

  function onCsWrite(_uc, _access, address, _size, value) {
    state.touched = true;
    if (Number(address) < CS || Number(address) > CS + 3) return;
    const v = Number(value) >>> 0;
    if (v & CLEAR_BIT) {
      state.tx = [];
      state.rx = [];
      state.cmd = 0;
      state.sDone = false;
    }
    const ta = (v & TA) !== 0;
    if (ta && !state.ta) {
      state.ta = true;
      process();
    }
    if (!ta) state.ta = false;
  }

  function onWrite() { state.touched = true; }

  uc.hook_add(ucMod.HOOK_MEM_WRITE, onFifoWrite, null, FIFO, FIFO + 4);
  uc.hook_add(ucMod.HOOK_MEM_WRITE, onCsWrite, null, CS, CS + 4);
  uc.hook_add(ucMod.HOOK_MEM_WRITE, onWrite, null, base, base + 0xFFF);

  function syncOut(uc) {
    if (!state.touched && state.inited) return;
    writeU32(uc, ENABLES, state.enabled ? 4 : 0);
    let cs = 0;
    cs |= S_TXD;
    if (state.rx.length > 0) cs |= S_RXD;
    if (state.sDone) cs |= S_DONE;
    if (state.ta) cs |= TA;
    writeU32(uc, CS, cs);
    if (state.rx.length > 0) {
      const buf = new Uint8Array(4);
      for (let i = 0; i < 4; i++) buf[i] = state.rx[i] || 0;
      state.guard = true;
      uc.mem_write(FIFO, buf);
      state.guard = false;
    }
    state.inited = true;
    state.touched = false;
  }

  function syncIn(uc) {
    if (!state.touched) return;
    if (readU32(uc, ENABLES) & 4) state.enabled = true;
    state.clk = readU32(uc, CLK);
    state.dlen = readU32(uc, DLEN);
  }

  function bridgeRx(data) {
    if (data && data.bytes) {
      state.rx = data.bytes.slice();
      state.sDone = true;
    }
  }

  return { state, syncOut, syncIn, bridgeRx };
}
