// BCM2835 On-chip temperature sensor (0x3F104004, part of the RNG block).
// Reads return a fixed realistic temperature (45.0 °C = 45000 millidegrees).
//
// Performance: dirty-flag gated — shares the RNG's write hook via the
// peers mechanism. syncOut only writes the DATA register on first init
// or when the RNG block was touched.

import { writeU32 } from './perf.js';

export function createTempSensor(base, rngState) {
  const DATA = base + 0x04;

  const state = {
    temperature: 45000,
    touched: true, // start dirty so first syncOut writes
    inited: false,
  };

  // Piggyback on RNG's dirty flag (same MMIO window)
  if (rngState) rngState.peers = [...(rngState.peers || []), state];

  function syncOut(uc) {
    if (!state.touched && state.inited) return;
    writeU32(uc, DATA, state.temperature);
    state.inited = true;
    state.touched = false;
  }

  function syncIn(_uc) {}

  return { state, syncOut, syncIn };
}
