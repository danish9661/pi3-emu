// Host-arbitrated BCM2835 SDHCI (EMMC) controller at 0x3F300000, backed by
// a host-played microSD card: a 40-sector FAT12 disk image holding one
// file, HELLO.TXT ("hello from the SD card"). The guest runs the real card
// init sequence and CMD17 single-block reads (programs/sd).
//
//   +0x00 ARG          command argument
//   +0x04 CMD          start bit 6 + index bits 5-0. Observed with a
//                      range-limited HOOK_MEM_WRITE: every guest write is a
//                      command execution (the same CMD value recurs, so a
//                      window diff could not detect it). The host only
//                      updates state in the hook; windows are refreshed in
//                      syncOut, never from inside the hook.
//   +0x10 RESP0..+0x1C response words (per command, see exec())
//   +0x20 DATA         real SDHCI position; unused here (see +0x100)
//   +0x30 INTERRUPT    CMD_COMPLETE bit 0, BUFFER_READ_READY bit 5;
//                      the guest clears by writing 1 (write-1-to-clear)
//   +0x34 IRPT_EN      interrupt status enable (R/W): raw bits are shown in
//                      the +0x30 window AND drive the IRQ line only while
//                      enabled here (real SDHCI semantics; the +0x30 window
//                      shows the raw status so the sd guest's poll keeps
//                      working — Linux programs both registers explicitly)
//   +0x38 IRPT_MASK    interrupt signal enable (R/W): the IRQ line is
//                      (raw & IRPT_EN & IRPT_MASK) != 0
//   +0x54 DONE         host extension: the guest writes 1 when finished
//   +0x100 BLOCK       512-byte block buffer (model detail: the real
//                      controller pops the data FIFO at +0x20 on every
//                      read; a plain window cannot pop, so the host loads
//                      the block here and the guest walks the addresses)
//
// Response map (the classic SD init sequence):
//   CMD0  -> R0 0x00000000     CMD2  -> CID (4 words)
//   CMD8  -> R7 0x000001AA     CMD3  -> R6 0x12340000 (RCA)
//   CMD55 -> R1 0x00000120     CMD7  -> R1 0x00000900
//   ACMD41-> R3 0xC0FF8000     CMD17 -> R1 0x00000900 + DATA block

const SEC = 512;
const CID = [0x12345678, 0x9abcdef0, 0x13579bdf, 0x2468ace0];

function makeDisk() {
  // FAT12 geometry: 1 reserved sector, 2 FATs of 1 sector, 16 root
  // entries (1 sector), 1 data sector -> sectors 0..4.
  const msg = "hello from the SD card\r\n";
  const s = [];
  for (let i = 0; i < 5; i++) s.push(new Uint8Array(SEC));

  const b = s[0];
  b[0] = 0xeb;
  b[1] = 0x3c;
  b[2] = 0x90;
  for (let i = 0; i < 8; i++) b[3 + i] = "PI3EMU  ".charCodeAt(i);
  b[11] = 0x00;
  b[12] = 0x02; // bytes per sector 512
  b[13] = 1; // sectors per cluster
  b[14] = 1; // reserved sectors
  b[15] = 0;
  b[16] = 2; // FAT count
  b[17] = 16; // root entries
  b[18] = 0;
  b[19] = 0x28; // total sectors 40
  b[20] = 0;
  b[21] = 0xf8; // media
  b[22] = 1; // sectors per FAT
  b[23] = 0;
  b[24] = 1; // sectors per track
  b[25] = 0;
  b[26] = 1; // heads
  b[27] = 0;
  b[29] = 0;
  b[33] = 0x28; // total sectors (again)
  b[34] = 0;

  const f = s[1];
  f[0] = 0xf8;
  f[1] = 0xff;
  f[2] = 0xff;
  f[3] = 0xff;
  f[4] = 0xff;
  f[5] = 0x0f; // entries 2 and 3 = 0xFFF (EOC)
  s[2].set(f); // second FAT copy

  const r = s[3];
  for (let i = 0; i < 11; i++) r[i] = "HELLO   TXT".charCodeAt(i);
  r[11] = 0x20; // archive
  r[20] = 2; // first cluster low word (FAT12 dir entry offset 0x14)
  r[30] = msg.length & 0xff; // file size
  r[31] = (msg.length >> 8) & 0xff;

  for (let i = 0; i < msg.length; i++) s[4][i] = msg.charCodeAt(i);
  return s;
}

export function createSdhci(uc, ucMod, base, onIrqChange) {
  const ARG = base + 0x00;
  const CMD = base + 0x04;
  const BLOCK = base + 0x100;
  const INTERRUPT = base + 0x30;
  const IRPT_EN = base + 0x34;
  const IRPT_MASK = base + 0x38;
  const DONE = base + 0x54;

  const IRPT_CMD_COMPLETE = 1;
  const IRPT_BUF_READ_READY = 1 << 5;

  const disk = makeDisk();

  const state = {
    base,
    resp: [0, 0, 0, 0],
    irq: 0,
    block: null, // last block loaded into DATA (CMD17)
    blockDirty: false,
    commands: [], // executed [index, arg] pairs (probe/status)
    done: false,
    intEn: 0,
    sigEn: 0,
  };

  function setResp(...w) {
    state.resp = [w[0] || 0, w[1] || 0, w[2] || 0, w[3] || 0];
  }

  // State-only: never touches the emulator (safe from inside the hook).
  function exec(index, arg) {
    state.commands.push([index, arg]);
    switch (index) {
      case 0:
        setResp(0); // CMD0 idle
        break;
      case 8:
        setResp(0x1aa); // CMD8 echo
        break;
      case 55:
        setResp(0x120); // APP_CMD
        break;
      case 41:
        setResp(0xc0ff8000); // ACMD41: ready, HCS, 3.3V
        break;
      case 2:
        setResp(CID[0], CID[1], CID[2], CID[3]);
        break;
      case 3:
        setResp(0x12340000); // RCA 0x1234
        break;
      case 7:
        setResp(0x900); // select: R1 ready
        break;
      case 17: {
        setResp(0x900);
        state.block = disk[arg & 0xffff] || new Uint8Array(SEC);
        state.blockDirty = true;
        state.irq |= IRPT_BUF_READ_READY;
        break;
      }
      default:
        setResp(0);
    }
    state.irq |= IRPT_CMD_COMPLETE;
  }

  // Fires only for guest writes: every CMD write executes the command.
  uc.hook_add(
    ucMod.HOOK_MEM_WRITE,
    (u, access, addr, size, value) => {
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

  function syncOut(uc) {
    for (let i = 0; i < 4; i++) {
      writeU32(uc, base + 0x10 + i * 4, state.resp[i]);
    }
    writeU32(uc, INTERRUPT, state.irq);
    writeU32(uc, IRPT_EN, state.intEn);
    writeU32(uc, IRPT_MASK, state.sigEn);
    if (state.blockDirty) {
      uc.mem_write(BLOCK, state.block);
      state.blockDirty = false;
    }
  }

  // Write-1-to-clear on the raw status (state-only: guest hook + probes).
  function w1c(mask) {
    const w = mask & state.irq;
    if (w) state.irq &= ~w;
  }

  function syncIn(uc) {
    // The INTERRUPT W1C happens in the write hook (guest accesses only) —
    // a window pull here would self-clear the host's own mirror write.
    state.intEn = readU32(uc, IRPT_EN);
    state.sigEn = readU32(uc, IRPT_MASK);
    if (readU32(uc, DONE) !== 0) state.done = true;
    if (onIrqChange) onIrqChange();
  }

  // The IRQ line into the legacy IC bank-2 bit 30 (GPU IRQ 62): raw bits
  // must be both status-enabled and signal-enabled (real SDHCI semantics).
  const irqActive = () => (state.irq & state.intEn & state.sigEn) !== 0;

  return { state, syncOut, syncIn, irqActive, exec, w1c };
}

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}

function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}
