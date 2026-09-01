// BCM2835 Hardware RNG (0x3F104000): produces 32-bit random numbers.
// The real controller uses an entropy source; here we use Math.random()
// seeded from crypto.getRandomValues for decent randomness.
//
//   +0x00 CTRL     bit 0 EN (enable), bit 1 SFTRST (auto-clear)
//   +0x04 STATUS   bits 0-7 data count in FIFO (0 or 1), bit 8 writable
//   +0x08 DATA     read: pulls a 32-bit random word (FIFO pop)
//   +0x0C INTACK   write-1-to-acknowledge
//   +0x10 INTEN    bit 0: data-ready interrupt enable

export function createRng(uc, ucMod, base) {
  const CTRL = base + 0x00;
  const STATUS = base + 0x04;
  const DATA = base + 0x08;
  const INTACK = base + 0x0c;
  const INTEN = base + 0x10;

  const state = {
    enabled: false,
    fifo: [],
    inten: 0,
    dataReady: false,
  };

  function generateWord() {
    const arr = new Uint32Array(1);
    crypto.getRandomValues(arr);
    return arr[0];
  }

  // Pre-fill FIFO with 1 word so the first read succeeds immediately.
  state.fifo.push(generateWord());

  function syncOut(uc) {
    const status = state.fifo.length > 0 ? 1 : 0;
    writeU32(uc, STATUS, status);
    if (state.fifo.length > 0) {
      state.dataReady = true;
    }
    if (state.dataReady) {
      writeU32(uc, DATA, state.fifo[0] || 0);
    }
  }

  function syncIn(uc) {
    const ctrl = readU32(uc, CTRL);
    state.enabled = (ctrl & 1) !== 0;
    state.inten = readU32(uc, INTEN) & 1;
    // ACK clears data-ready
    const ack = readU32(uc, INTACK);
    if (ack & 1) state.dataReady = false;
    // Guest reads DATA — pop the FIFO
    if (state.enabled) {
      const d = readU32(uc, DATA);
      if (state.fifo.length > 0) state.fifo.shift();
      // Refill
      state.fifo.push(generateWord());
    }
  }

  return { state, syncOut, syncIn };
}

function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}
