// BCM2835 SDHCI (EMMC) controller at 0x3F300000.
//
// Supports PIO mode (block buffer at +0x100) and SDMA mode (single-block
// and multi-block DMA to guest RAM). The guest writes the DMA target
// address to the DONE extension register (host extension, not standard
// SDHCI) and sets TRANSFER_MODE bit 0 to enable DMA.
//
// Register map (BCM2835-style, matching the sd bare-metal guest):
//   +0x00  ARG           command argument
//   +0x04  CMD           start bit (6) | index (5-0). Every guest write
//                        executes a command.
//   +0x10  RESP0..+0x1C response words
//   +0x24  PRESENT_STATE
//   +0x2C  CLOCK_CONTROL
//   +0x30  INTERRUPT_STATUS   R/W1C
//   +0x34  IRPT_EN       interrupt status enable
//   +0x38  IRPT_MASK     interrupt signal enable
//   +0x40  CAPABILITIES0 bit 10=SDMA, bit 15=HCS
//   +0x54  DONE          host extension: guest parks when done;
//                        ALSO used as DMA address register (guest writes
//                        the physical address here before CMD17/18 with
//                        TRANSFER_MODE bit 0 set).
//   +0x100 BLOCK_DATA    512-byte block buffer (PIO mode)

const SEC = 512;
const CID = [0x12345678, 0x9abcdef0, 0x13579bdf, 0x2468ace0];

function makeDisk() {
  const msg = "hello from the SD card\r\n";
  const s = [];
  for (let i = 0; i < 5; i++) s.push(new Uint8Array(SEC));
  const b = s[0];
  b[0] = 0xeb; b[1] = 0x3c; b[2] = 0x90;
  for (let i = 0; i < 8; i++) b[3 + i] = "PI3EMU  ".charCodeAt(i);
  b[11] = 0x00; b[12] = 0x02; b[13] = 1; b[14] = 1;
  b[16] = 2; b[17] = 16; b[19] = 0x28; b[21] = 0xf8;
  b[22] = 1; b[24] = 1; b[26] = 1; b[33] = 0x28;
  const f = s[1];
  f[0] = 0xf8; f[1] = 0xff; f[2] = 0xff; f[3] = 0xff;
  f[4] = 0xff; f[5] = 0x0f;
  s[2].set(f);
  const r = s[3];
  for (let i = 0; i < 11; i++) r[i] = "HELLO   TXT".charCodeAt(i);
  r[11] = 0x20; r[20] = 2; r[30] = msg.length & 0xff; r[31] = (msg.length >> 8) & 0xff;
  for (let i = 0; i < msg.length; i++) s[4][i] = msg.charCodeAt(i);
  return s;
}

import { readU32, writeU32 } from './perf.js';

export function createSdhci(uc, ucMod, base, onIrqChange) {
  const ARG           = base + 0x00;
  const CMD           = base + 0x04;
  const RESP0         = base + 0x10;
  const RESP1         = base + 0x14;
  const RESP2         = base + 0x18;
  const RESP3         = base + 0x1C;
  const PRESENT_STATE = base + 0x24;
  const CLOCK_CTL     = base + 0x2C;
  const INTERRUPT     = base + 0x30;
  const IRPT_EN       = base + 0x34;
  const IRPT_MASK     = base + 0x38;
  const CAP0          = base + 0x40;
  const CAP1          = base + 0x44;
  const DONE          = base + 0x54;
  const BLOCK_DATA    = base + 0x100;
  const SLOT_INT      = base + 0xFC;
  const TRANSFER_MODE = base + 0x0C;

  const IRPT_CMD_COMPLETE    = 1 << 0;
  const IRPT_XFER_COMPLETE   = 1 << 1;
  const IRPTBUF_READ_READY   = 1 << 5;
  const IRPT_BUF_WRITE_READY = 1 << 4;
  const IRPT_DMA             = 1 << 3;
  const IRPT_ERROR           = 1 << 15;

  const disk = makeDisk();

  const state = {
    resp: [0, 0, 0, 0],
    irq: 0,
    block: null,
    blockDirty: false,
    commands: [],
    done: false,
    intEn: 0,
    sigEn: 0,
    blockLen: 512,
    blockCountTotal: 0,
    transferMode: 0,
    presentState: 0x00010000,
    clockCtl: 0,
    touched: false,
    inited: false,
    dmaAddr: 0,
    dmaActive: false,
  };

  function setResp(...w) {
    state.resp = [w[0] || 0, w[1] || 0, w[2] || 0, w[3] || 0];
  }

  function readSector(sec) {
    return disk[sec & 0xffff] || new Uint8Array(SEC);
  }

  function exec(index, arg) {
    state.commands.push([index, arg]);
    switch (index) {
      case 0: setResp(0); break;
      case 1: setResp(0x80ff8080); break;
      case 2: setResp(CID[0], CID[1], CID[2], CID[3]); break;
      case 3: setResp(0x12340000); break;
      case 6: setResp(0x900); break;
      case 7: setResp(0x900); break;
      case 8: setResp(0x1aa); break;
      case 9: setResp(0x900); break;
      case 12: setResp(0x900); break;
      case 13: setResp(0x900); break;
      case 16: {
        state.blockLen = arg & 0x7ff;
        setResp(0x900);
        break;
      }
      case 17: {
        setResp(0x900);
        const sector = arg & 0xffff;
        if (state.dmaActive && state.dmaAddr) {
          const data = readSector(sector);
          try { uc.mem_write(state.dmaAddr, data); } catch (_) {}
          state.irq |= IRPT_XFER_COMPLETE;
        } else {
          state.block = readSector(sector);
          state.blockDirty = true;
          state.irq |= IRPTBUF_READ_READY;
          state.irq |= IRPT_XFER_COMPLETE;
        }
        break;
      }
      case 18: {
        setResp(0x900);
        const startSector = arg & 0xffff;
        const count = state.blockCountTotal || 1;
        if (state.dmaActive && state.dmaAddr) {
          for (let i = 0; i < count; i++) {
            const data = readSector(startSector + i);
            try { uc.mem_write(state.dmaAddr + i * SEC, data); } catch (_) {}
          }
          state.irq |= IRPT_XFER_COMPLETE | IRPT_DMA;
        } else {
          state.block = readSector(startSector);
          state.blockDirty = true;
          state.irq |= IRPTBUF_READ_READY | IRPT_XFER_COMPLETE;
        }
        break;
      }
      case 55: setResp(0x120); break;
      case 41: setResp(0xc0ff8000); break;
      case 51: {
        setResp(0x900);
        state.block = new Uint8Array(SEC);
        state.block[0] = 0x02;
        state.blockDirty = true;
        state.irq |= IRPTBUF_READ_READY;
        break;
      }
      case 52: setResp(0x900); break;
      case 53: setResp(0x900); break;
      default: setResp(0);
    }
    state.irq |= IRPT_CMD_COMPLETE;
  }

  // Unified write hook: CMD write -> execute command, INTERRUPT write -> W1C
  uc.hook_add(
    ucMod.HOOK_MEM_WRITE,
    (u, access, addr, size, value) => {
      state.touched = true;
      const a = Number(addr);
      const v = Number(value);
      if (a >= INTERRUPT && a < INTERRUPT + 4) {
        state.irq &= ~v;
      } else {
        exec(v & 0x3f, readU32(uc, ARG));
      }
      if (onIrqChange) onIrqChange();
    },
    null,
    CMD,
    INTERRUPT + 3
  );

  // Track DMA address writes to the DONE register (host extension).
  // Guest writes DMA target address before CMD17/18 with DMA enabled.
  // Guest writes 1 to DONE when finished (park).
  uc.hook_add(
    ucMod.HOOK_MEM_WRITE,
    (_u, _access, addr, _size, value) => {
      const v = Number(value) >>> 0;
      state.touched = true;
      if (v > 0x1000) {
        state.dmaAddr = v;
        state.dmaActive = true;
      } else {
        state.done = true;
      }
    },
    null,
    DONE,
    DONE + 3
  );

  function syncOut(uc) {
    if (!state.touched && state.inited) return;
    writeU32(uc, RESP0, state.resp[0]);
    writeU32(uc, RESP1, state.resp[1]);
    writeU32(uc, RESP2, state.resp[2]);
    writeU32(uc, RESP3, state.resp[3]);
    writeU32(uc, INTERRUPT, state.irq);
    writeU32(uc, IRPT_EN, state.intEn);
    writeU32(uc, IRPT_MASK, state.sigEn);
    let ps = 0x00010000;
    if (state.blockDirty || (state.irq & IRPTBUF_READ_READY)) ps |= 1 << 17;
    if (!(state.irq & IRPT_CMD_COMPLETE)) ps |= 1 << 0;
    writeU32(uc, PRESENT_STATE, ps);
    writeU32(uc, CAP0, 0x00000407);
    writeU32(uc, CAP1, 0x00000000);
    writeU32(uc, CLOCK_CTL, state.clockCtl);
    writeU32(uc, SLOT_INT, 0x0001);
    if (state.blockDirty && state.block) {
      uc.mem_write(BLOCK_DATA, state.block);
      state.blockDirty = false;
    }
    state.inited = true;
    state.touched = false;
  }

  function syncIn(uc) {
    state.intEn = readU32(uc, IRPT_EN);
    state.sigEn = readU32(uc, IRPT_MASK);
    if (readU32(uc, DONE) !== 0 && readU32(uc, DONE) <= 0x1000) state.done = true;
    if (onIrqChange) onIrqChange();
  }

  const irqActive = () => (state.irq & state.intEn & state.sigEn) !== 0;

  return { state, syncOut, syncIn, irqActive, exec, w1c: (mask) => { state.irq &= ~mask; } };
}
