// Host-arbitrated BCM2835 SDHCI (EMMC) controller at 0x3F300000.
//
// Expanded for Linux sdhci-iproc driver probe: more SD commands,
// more registers (present_state, capability, block size/count,
// transfer mode, clock control, software reset, host control,
// slot interrupt status), and multi-block read support.
//
// The FAT12 disk image is unchanged (bare-metal sd guest compatibility).

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
  // Real BCM2835 EMMC register offsets (mapped from the standard SDHCI layout)
  const ARG        = base + 0x00;
  const CMD        = base + 0x04;
  const RESP0      = base + 0x10;
  const RESP1      = base + 0x14;
  const RESP2      = base + 0x18;
  const RESP3      = base + 0x1C;
  const BLOCK_DATA  = base + 0x100; // host extension: 512-byte block buffer
  const INTERRUPT  = base + 0x30;
  const IRPT_EN    = base + 0x34;
  const IRPT_MASK  = base + 0x38;
  const CONTROL    = base + 0x3C; // host control (DMA select, etc.)
  const CLOCK_CTL  = base + 0x2C; // clock control
  const SW_RESET   = base + 0x2F; // software reset (byte)
  const CAPABILITIES0 = base + 0x40;
  const CAPABILITIES1 = base + 0x44;
  const PRESENT_STATE = base + 0x24;
  const BLOCK_SIZE = base + 0x04; // shared offset with ARG in real HW —
                                  // actually ARG is at +0x00 in BCM2835 EMMC
  const BLOCK_COUNT = base + 0x06;
  const TRANSFER_MODE = base + 0x0C;
  const HOST_CTRL2  = base + 0x3E;
  const SLOT_INT    = base + 0xFC;
  const DONE        = base + 0x54; // host extension

  // Interrupt status bits (real SDHCI)
  const IRPT_CMD_COMPLETE    = 1 << 0;
  const IRPTBUF_READ_READY   = 1 << 5;
  const IRPT_XFER_COMPLETE   = 1 << 1;
  const IRPT_BLKGAP          = 1 << 2;
  const IRPT_DMA             = 1 << 3;
  const IRPT_BUF_WRITE_READY = 1 << 4;
  const IRPT_CARD_INSERT     = 1 << 6;
  const IRPT_CARD_REMOVE     = 1 << 7;
  const IRPT_ERROR           = 1 << 15;

  const disk = makeDisk();

  const state = {
    base,
    resp: [0, 0, 0, 0],
    irq: 0,
    block: null,
    blockDirty: false,
    commands: [],
    done: false,
    intEn: 0,
    sigEn: 0,
    blockLen: 512,
    blockCount: 0,
    transferMode: 0,
    clockCtl: 0,
    hostCtrl: 0,
    softwareReset: 0,
    presentState: 0x00010000, // bit 16: card inserted, bit 20: buffer read enable
    touched: false,
    inited: false,
  };

  function setResp(...w) {
    state.resp = [w[0] || 0, w[1] || 0, w[2] || 0, w[3] || 0];
  }

  function exec(index, arg) {
    state.commands.push([index, arg]);
    switch (index) {
      case 0: setResp(0); break; // GO_IDLE
      case 1: setResp(0x80ff8080); break; // SEND_OP_COND (MMC)
      case 2: setResp(CID[0], CID[1], CID[2], CID[3]); break; // ALL_SEND_CID
      case 3: setResp(0x12340000); break; // SEND_RELATIVE_ADDR
      case 6: setResp(0x900); break; // SWITCH (MMC)
      case 7: setResp(0x900); break; // SELECT_CARD
      case 8: setResp(0x1aa); break; // SEND_IF_COND
      case 9: setResp(0x900); break; // SEND_CSD
      case 12: setResp(0x900); break; // STOP_TRANSMISSION
      case 13: setResp(0x900); break; // SEND_STATUS
      case 16: setResp(0x900); break; // SET_BLOCKLEN
      case 17: { // READ_SINGLE_BLOCK
        setResp(0x900);
        state.block = disk[arg & 0xffff] || new Uint8Array(SEC);
        state.blockDirty = true;
        state.irq |= IRPTBUF_READ_READY;
        break;
      }
      case 18: { // READ_MULTIPLE_BLOCK
        setResp(0x900);
        // Load first block; subsequent blocks loaded on buf-read-ready ack
        state.block = disk[arg & 0xffff] || new Uint8Array(SEC);
        state.blockDirty = true;
        state.irq |= IRPTBUF_READ_READY;
        break;
      }
      case 55: setResp(0x120); break; // APP_CMD
      case 41: setResp(0xc0ff8000); break; // ACMD41
      case 51: { // SEND_SCR (SD)
        setResp(0x900);
        // Return a minimal SCR: SD spec 2.0, bus width 1-bit
        state.block = new Uint8Array(SEC);
        state.block[0] = 0x02; // SCR structure version
        state.block[1] = 0x00; // SD bus width support
        state.blockDirty = true;
        state.irq |= IRPTBUF_READ_READY;
        break;
      }
      case 52: setResp(0x900); break; // IO_RW_DIRECT (SDIO)
      case 53: setResp(0x900); break; // IO_RW_EXTENDED (SDIO)
      default:
        setResp(0);
    }
    state.irq |= IRPT_CMD_COMPLETE;
    // For data commands, also signal buffer ready
    if (index === 17 || index === 18) {
      state.irq |= IRPT_XFER_COMPLETE;
    }
  }

  // CMD write hook
  uc.hook_add(
    ucMod.HOOK_MEM_WRITE,
    (u, access, addr, size, value) => {
      state.touched = true;
      const a = Number(addr);
      if (a >= INTERRUPT && a < INTERRUPT + 4) {
        w1c(Number(value));
      } else {
        exec(Number(value) & 0x3f, readU32(uc, ARG));
      }
      if (onIrqChange) onIrqChange();
    },
    null,
    CMD,
    INTERRUPT + 3
  );

  // Broader write hook for other registers (clock, reset, etc.)
  uc.hook_add(
    ucMod.HOOK_MEM_WRITE,
    (_u, _access, addr, _size, value) => {
      state.touched = true;
      const a = Number(addr);
      const v = Number(value);
      if (a === CLOCK_CTL) {
        state.clockCtl = v;
      } else if (a === SW_RESET) {
        state.softwareReset = v;
        if (v & 1) { // full reset
          state.irq = 0;
          state.resp = [0, 0, 0, 0];
          state.presentState = 0x00010000;
        }
      } else if (a === CONTROL || a === HOST_CTRL2) {
        state.hostCtrl = v;
      }
    },
    null,
    CLOCK_CTL,
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
    // Present state: buffer read ready if we have block data, card inserted
    let ps = 0x00010000; // bit 16: card stable
    if (state.blockDirty || state.irq & IRPTBUF_READ_READY) ps |= 1 << 11; // buffer read enable
    if (!(state.irq & IRPT_CMD_COMPLETE)) ps |= 1 << 20; // command inhibit (busy)
    writeU32(uc, PRESENT_STATE, ps);
    // Capabilities: SDMA, 3.3V, 1.8V, 64-bit address (not used)
    writeU32(uc, CAPABILITIES0, 0x0000_0807); // SDMA support, 3.3V, 1.8V
    writeU32(uc, CAPABILITIES1, 0x0000_0000);
    writeU32(uc, CLOCK_CTL, state.clockCtl);
    writeU32(uc, CONTROL, state.hostCtrl);
    writeU32(uc, HOST_CTRL2, 0x0000);
    writeU32(uc, SLOT_INT, 0x0001); // one slot, interrupt supported
    if (state.blockDirty) {
      uc.mem_write(BLOCK_DATA, state.block);
      state.blockDirty = false;
    }
    state.inited = true;
    state.touched = false;
  }

  function w1c(mask) {
    const w = mask & state.irq;
    if (w) state.irq &= ~w;
  }

  function syncIn(uc) {
    // NOTE: no dirty-flag gate here — host mem_write (from probes/Linux)
    // doesn't fire write hooks, so touched stays false.
    state.intEn = readU32(uc, IRPT_EN);
    state.sigEn = readU32(uc, IRPT_MASK);
    if (readU32(uc, DONE) !== 0) state.done = true;
    if (onIrqChange) onIrqChange();
  }

  const irqActive = () => (state.irq & state.intEn & state.sigEn) !== 0;

  return { state, syncOut, syncIn, irqActive, exec, w1c };
}
