// Host-arbitrated BCM2835 SPI0 master (0x3F204000), with the host playing
// an SPI slave: a flash chip that answers the JEDEC ID command (0x9F) with
// 0xEF 0x40 0x18 (e.g. Winbond W25Q128). Each transaction runs like a real
// SPI0 transfer: the guest pushes command bytes into the FIFO, raises TA,
// polls CS.DONE, reads the response from the FIFO, then CLEARs the FIFOs.
//
//   +0x00 CS     CS0 bit0, TA bit7 (transfer active), CLEAR bits 4-5 (edge:
//                resets the FIFOs and the session), TXD bit18, RXD bit17,
//                DONE bit16 (host: response ready)
//   +0x04 FIFO   the guest writes command bytes (write hook, like the PWM
//                FIFO); the host loads the response into the window
//   +0x54 DONE   host extension: the guest writes 1 when finished
//
// The write hook fires for the guest's FIFO pushes only; the host guards
// its own response writes (state.guard) so they don't re-enter the TX
// queue. CLEAR (guest) resets the session so repeated transactions work.

export function createSpi(uc, ucMod, base, onBridgeData) {
  const CS = base + 0x00;
  const FIFO = base + 0x04;
  const DONE = base + 0x54;

  const TA = 1 << 7;
  const CLEAR_MASK = 0b11 << 4;
  const S_DONE = 1 << 16;
  const S_TXD = 1 << 18;

  const state = {
    base,
    tx: [], // outbound bytes (guest -> slave)
    rx: [], // response bytes (slave -> guest)
    cmd: 0, // first byte of the transaction (the command)
    ta: false,
    sDone: false, // CS.DONE (response ready)
    done: false, // host-extension DONE
    guard: false, // host is writing the window: ignore the write hook
  };

  // Responses per outbound byte index: byte 0 = the command (dummy cycle
  // response 0x00), then the JEDEC ID, then 0xFF.
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
      const idx = state.tx.length - 1;
      state.rx.push(slaveResponse(state.cmd, idx));
    }
  }

  function process() {
    if (state.tx.length > 0) {
      if (onBridgeData) {
        // Bidirectional bridge: forward TX bytes to browser, defer DONE
        // until bridgeRx() provides the MISO response.
        state.sDone = false;
        onBridgeData({ type: 'spi-tx', bytes: state.tx.slice() });
      } else {
        state.sDone = true;
      }
    }
  }

  // Fires only for guest writes (the host sets state.guard when it writes
  // the response into the FIFO window). The unicorn hook range end is
  // inclusive, so guard by address too: the CS hook below would otherwise
  // see FIFO writes as well.
  function onFifoWrite(uc2, access, address, size, value) {
    if (state.guard) return;
    if (Number(address) < FIFO || Number(address) > FIFO + 3) return;
    const v = Number(value) >>> 0;
    if (state.tx.length === 0) state.cmd = v & 0xff; // first byte = command
    pushTx(size, v);
  }

  // The CS register is write-hooked for the same reason as the FIFO: a
  // window diff only sees the LAST write of a slice, so a CLEAR followed
  // by TA in the same slice would look like just TA (and vice versa). The
  // hook sees every write, so CLEAR resets and TA edges are exact.
  function onCsWrite(uc2, access, address, size, value) {
    if (Number(address) < CS || Number(address) > CS + 3) return;
    const v = Number(value) >>> 0;
    if (v & CLEAR_MASK) {
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

  const hook = ucMod.HOOK_MEM_WRITE;
  uc.hook_add(hook, onFifoWrite, null, FIFO, FIFO + 4);
  uc.hook_add(hook, onCsWrite, null, CS, CS + 4);

  function syncOut(uc) {
    let cs = state.ta ? TA : 0;
    cs |= S_TXD; // TX fifo always empty (host drains instantly)
    if (state.rx.length > 0) cs |= 1 << 17; // RXD
    if (state.sDone) cs |= S_DONE;
    writeU32(uc, CS, cs);
    // Load the response into the FIFO window for the guest to read. The
    // guest's TX writes clobber this window mid-slice, but it can only
    // read the response after DONE (set at a slice boundary), by which
    // point this reload has run.
    if (state.rx.length > 0) {
      const buf = new Uint8Array(4);
      for (let i = 0; i < 4; i++) buf[i] = state.rx[i] || 0;
      state.guard = true;
      uc.mem_write(FIFO, buf);
      state.guard = false;
    }
  }

  function syncIn(uc) {
    if (readU32(uc, DONE) !== 0) state.done = true;
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
