// BCM2835 I2S (PCM) audio interface (0x3F203000): serial audio bus used
// for HDMI audio, codec connections, and Hifiberry-style DACs.
//
// Real register layout:
//   +0x00 CS_A      Control/Status
//     bit 0: EN (enable)
//     bit 1: TXON (TX operating)
//     bit 2: RXON (RX operating)
//     bit 3: TXTHR  (TX threshold, not modelled precisely)
//     bit 4: RXTHR  (RX threshold)
//     bit 9: TXERR  (TX FIFO error, write-1-to-clear)
//     bit 10: RXERR (RX FIFO error, W1C)
//     bit 15: TXW    (TX writable, host: 1 when FIFO not full)
//     bit 16: RXR    (RX readable, host: 0 when FIFO empty)
//     bit 24: TXD    (TX DMA DREQ)
//     bit 25: RXD    (RX DMA DREQ)
//     bit 26: TXE    (TX FIFO empty)
//     bit 27: RXF    (RX FIFO full)
//
//   +0x04 FIFO_A   FIFO access (write: TX push, read: RX pop)
//   +0x08 MODE_A   Mode: clock pack/sync width
//   +0x0C RXC_A    RX clock config
//   +0x10 TXC_A    TX clock config
//   +0x14 DREQ_A   DMA request levels
//   +0x18 INTEN_A  Interrupt enable
//   +0x1C INTSTC_A Interrupt status/clear
//   +0x20 GRAY     Gray code (not modelled)

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
  };

  function syncOut(uc) {
    if (!state.enabled) {
      writeU32(uc, CS_A, 0);
      return;
    }
    let cs = state.cs & (EN | TXON | RXON);
    cs |= TXW | TXE; // TX FIFO always empty (not drained by codec)
    if (state.rxFifo.length > 0) cs |= RXR; // RX has data to read
    cs &= ~(1 << 9 | 1 << 10); // clear error bits
    writeU32(uc, CS_A, cs);
    if (state.txFifo.length > 0) {
      writeU32(uc, FIFO_A, state.txFifo[0]);
    }
  }

  function syncIn(uc) {
    state.cs = readU32(uc, CS_A);
    state.enabled = (state.cs & EN) !== 0;
    state.mode = readU32(uc, MODE_A);
    state.rxc = readU32(uc, RXC_A);
    state.txc = readU32(uc, TXC_A);
    state.dreq = readU32(uc, DREQ_A);
    state.inten = readU32(uc, INTEN_A);
    // W1C: clear RXERR/TXERR if guest wrote them
    const ack = readU32(uc, INTSTC_A);
    if (ack & (1 << 9)) state.cs &= ~(1 << 9);
    if (ack & (1 << 10)) state.cs &= ~(1 << 10);
  }

  // Guest pushes TX data via the FIFO write hook
  function onFifoWrite(_uc2, _access, _address, _size, value) {
    if (state.enabled) {
      state.txFifo.push(Number(value) >>> 0);
      if (state.txFifo.length > 64) state.txFifo.shift();
    }
  }

  uc.hook_add(ucMod.HOOK_MEM_WRITE, onFifoWrite, null, FIFO_A, FIFO_A + 4);

  return { state, syncOut, syncIn };
}

function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}
