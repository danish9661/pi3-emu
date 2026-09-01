// Mini UART2–5: additional serial consoles at custom bases.
//
// Performance: dirty-flag gated — syncOut/syncIn only run when the guest
// writes to the UART window. Since there are 4 instances, this saves
// ~16 wasm crossings per slice when idle.

import { readU32, writeU32 } from './perf.js';

export function createUart25(uc, ucMod, base, index, emit) {
  const IO = base + 0x40;
  const LSR = base + 0x54;
  const ENABLES = base + 0x04;

  const LSR_TX_EMPTY = 1 << 5;
  const LSR_TX_IDLE = 1 << 6;

  const state = { base, index, enabled: false, touched: false, inited: false };

  function onWrite() { state.touched = true; }
  uc.hook_add(ucMod.HOOK_MEM_WRITE, onWrite, null, base, base + 0xFFF);

  if (typeof emit === 'function') {
    uc.hook_add(
      ucMod.HOOK_MEM_WRITE,
      (_u, _access, _address, _size, value) => {
        state.touched = true;
        emit(index, Number(value) & 0xff);
      },
      null,
      IO,
      IO + 4
    );
  }

  function syncOut(uc) {
    if (!state.touched && state.inited) return;
    writeU32(uc, LSR, state.enabled ? LSR_TX_EMPTY | LSR_TX_IDLE : 0);
    writeU32(uc, ENABLES, state.enabled ? 1 : 0);
    writeU32(uc, IO, 0);
    state.inited = true;
    state.touched = false;
  }

  function syncIn(uc) {
    if (!state.touched) return;
    if (readU32(uc, ENABLES) !== 0) state.enabled = true;
  }

  return { state, syncOut, syncIn };
}
