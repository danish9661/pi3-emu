// BCM2835 AUX SPI1 (0x3F215080): the second SPI bus, part of the AUX
// peripheral block (same block as the mini UART). Shares the AUX_ENABLES
// register at +0x04.
//
// Real register layout:
//   +0x00 AUX_IRQ      (shared with mini UART, not modelled here)
//   +0x04 AUX_ENABLES   bit 1: SPI1 enable
//
//   +0x80 SPI1_CS       Control/Status
//     bit 0: RXR (RX FIFO needs reading)
//     bit 1: TXD (TX FIFO can accept data)
//     bit 2: RXD (RX FIFO has data)
//     bit 3: TXR (TX FIFO needs writing)
//     bit 7: DONE
//     bit 11-10: MODE (00=SPI)
//     bit 16: CLEAR (write 1 to clear FIFOs)
//     bit 24: TA (transfer active)
//   +0x84 SPI1_FIFO     TX/RX FIFO (write=TX push, read=RX pop)
//   +0x88 SPI1_CLK      Clock divider
//   +0x8C SPI1_DLEN     Data length
//   +0x90 SPI1_LOSSI    LoSSI mode
//   +0x94 SPI1_DC       DMA Control

export function createSpi1(uc, ucMod, base, onBridgeData) {
  const CS = base + 0x80;
  const FIFO = base + 0x84;
  const CLK = base + 0x88;
  const DLEN = base + 0x8c;
  const ENABLES = base + 0x04; // AUX_ENABLES (bit 1)

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
        onBridgeData({ type: 'spi1-tx', bytes: state.tx.slice() });
      } else {
        state.sDone = true;
      }
    }
  }

  function onFifoWrite(_uc, _access, address, size, value) {
    if (state.guard) return;
    if (Number(address) < FIFO || Number(address) > FIFO + 3) return;
    const v = Number(value) >>> 0;
    if (state.tx.length === 0) state.cmd = v & 0xff;
    pushTx(size, v);
  }

  function onCsWrite(_uc, _access, address, _size, value) {
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

  uc.hook_add(ucMod.HOOK_MEM_WRITE, onFifoWrite, null, FIFO, FIFO + 4);
  uc.hook_add(ucMod.HOOK_MEM_WRITE, onCsWrite, null, CS, CS + 4);

  function syncOut(uc) {
    writeU32(uc, ENABLES, state.enabled ? 2 : 0); // bit 1 = SPI1
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
  }

  function syncIn(uc) {
    if (readU32(uc, ENABLES) & 2) state.enabled = true;
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

function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}
