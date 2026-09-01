// Mini UART2–5: additional serial consoles at custom bases.
// Reuses the UART1 model pattern (output-only, one line prefix each).
// Each instance maps 4K at a unique base and exposes the standard
// AUX mini UART register set (MU_IO +0x40, LSR +0x54, ENABLES +0x04).
//
// The addresses below are soft: there are only UART0 (PL011) and
// UART1 (AUX) on real BCM2837, so these are placed at unused gaps in
// the peripheral space for demo/test purposes. Each one gets its own
// 4K window so unicorn can map it without aliasing.

export function createUart25(uc, ucMod, base, index, emit) {
  const IO = base + 0x40;
  const LSR = base + 0x54;
  const ENABLES = base + 0x04;

  const LSR_TX_EMPTY = 1 << 5;
  const LSR_TX_IDLE = 1 << 6;

  const state = { base, index, enabled: false };

  if (typeof emit === 'function') {
    uc.hook_add(
      ucMod.HOOK_MEM_WRITE,
      (_u, _access, _address, _size, value) => {
        emit(index, Number(value) & 0xff);
      },
      null,
      IO,
      IO + 4
    );
  }

  function syncOut(uc) {
    writeU32(uc, LSR, state.enabled ? LSR_TX_EMPTY | LSR_TX_IDLE : 0);
    writeU32(uc, ENABLES, state.enabled ? 1 : 0);
    writeU32(uc, IO, 0);
  }

  function syncIn(uc) {
    if (readU32(uc, ENABLES) !== 0) state.enabled = true;
  }

  return { state, syncOut, syncIn };
}

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}

function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}
