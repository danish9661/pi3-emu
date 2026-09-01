// BCM2835 On-chip temperature sensor (0x3F104004, part of the RNG block).
// Reads return a fixed realistic temperature (45.0 °C = 45000 millidegrees).
//   +0x00 (relative to RNG base 0x3F104000) = DATA register at 0x3F104004
//     bits 23-0: temperature in thousandths of a degree Celsius
//
// We share the RNG's MMIO window (same 4K page) so no separate mem_map needed.

export function createTempSensor(base) {
  const DATA = base + 0x04; // offset into the RNG block

  const state = {
    temperature: 45000, // 45.0 °C in millidegrees
  };

  function syncOut(uc) {
    writeU32(uc, DATA, state.temperature);
  }

  function syncIn(_uc) {
    // read-only: nothing to pull from guest
  }

  return { state, syncOut, syncIn };
}

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}
