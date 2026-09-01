// Host-arbitrated BCM2835 BSC master (I2C) at 0x3F804000, with the host
// playing an I2C slave: a small sensor at address 0x68 with registers
//  0x00 WHO_AM_I  -> 0x68
//  0x10 TEMP      -> 2 bytes: 26, 0  (degrees C)
//  0x20 COUNTER   -> increments per read (1, 2, 3, ...)
// The slave latches the last byte written in a write transfer as the
// register address, and serves the register's value on the next read
// transfer (a classic sensor sequence: write reg addr, read data).
//
//   +0x00 C     I2CEN bit15, ST bit7 (start, auto-clear), CLEAR bit4
//               (edge: clears FIFO + DONE), READ bit0 (read transfer)
//   +0x04 S     DONE bit7 (set when a transfer completes; cleared by CLEAR)
//   +0x08 DLEN  transfer length (bytes)
//   +0x0C A     slave address (7-bit)
//   +0x10 FIFO  data: the guest writes outgoing bytes, the host loads the
//               slave's response here for read transfers
//   +0x54 DONE  host extension: the guest writes 1 when finished
//
// Pure window model (like the DMA/IC): when C.ST rises, the host snapshots
// the FIFO window (DLEN bytes), runs the slave, and for reads writes the
// response into the FIFO window; the guest reads it directly.

export function createI2c(uc, ucMod, base, onBridgeData) {
  const C = base + 0x00;
  const S = base + 0x04;
  const DLEN = base + 0x08;
  const FIFO = base + 0x10;
  const DONE = base + 0x54;

  const I2CEN = 1 << 15;
  const ST = 1 << 7;
  const CLEAR = 1 << 4;
  const READ = 1 << 0;
  const S_DONE = 1 << 7;

  const state = {
    base,
    c: 0, // latched I2CEN|READ bits
    sDone: false, // transfer completed (S.DONE)
    dlen: 0,
    addr: 0,
    counter: 0,
    reg: 0, // slave register selected by the last write transfer
    done: false, // host-extension DONE
    lastC: 0, // canonical C the host last wrote back
  };

  function slaveRead(reg) {
    switch (reg) {
      case 0x00:
        return [0x68];
      case 0x10:
        return [26, 0];
      case 0x20:
        state.counter++;
        return [state.counter & 0xff];
      default:
        return [0];
    }
  }

  function syncOut(uc) {
    writeU32(uc, S, state.sDone ? S_DONE : 0);
    writeU32(uc, C, (state.c & (I2CEN | READ)) | (state.sDone ? ST : 0));
    if (state.sDone) {
      // Keep the response visible in the FIFO window for the guest to read.
      const resp = state.resp || [0];
      const buf = new Uint8Array(4);
      for (let i = 0; i < 4; i++) buf[i] = resp[i] || 0;
      uc.mem_write(FIFO, buf);
    }
  }

  function syncIn(uc) {
    const v = readU32(uc, C);
    if (v !== state.lastC) {
      state.lastC = v;
      if (v & CLEAR) {
        state.sDone = false;
        state.resp = [];
      }
      const start = (v & ST) !== 0 && (state.c & ST) === 0;
      state.c = v & (I2CEN | READ | ST);
      if (start) {
        state.dlen = readU32(uc, DLEN) & 0xffff;
        state.addr = readU32(uc, 0x0c) & 0x7f;
        if (v & READ) {
          if (onBridgeData) {
            // Bidirectional bridge: forward read request to browser,
            // defer sDone until bridgeRx() provides the response.
            state.sDone = false;
            onBridgeData({ type: 'i2c-read', addr: state.addr, reg: state.reg, dlen: state.dlen });
          } else {
            state.resp = slaveRead(state.reg);
            state.sDone = true;
          }
        } else {
          const n = Math.min(state.dlen, 4);
          const w = readU32(uc, FIFO);
          const bytes = [w & 0xff, (w >>> 8) & 0xff, (w >>> 16) & 0xff, (w >>> 24) & 0xff];
          state.reg = bytes[0] & 0xff;
          if (onBridgeData) {
            onBridgeData({ type: 'i2c-write', addr: state.addr, reg: state.reg, dlen: state.dlen });
          }
          state.sDone = true;
        }
      }
    }
    if (readU32(uc, DONE) !== 0) state.done = true;
  }

  function bridgeRx(data) {
    if (data && data.bytes) {
      state.resp = data.bytes.slice();
      state.sDone = true;
    }
  }

  return { state, syncOut, syncIn, bridgeRx };
}

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}

function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}
