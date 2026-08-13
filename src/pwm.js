// Host-arbitrated BCM2835 PWM controller (FIFO-mode model) at 0x3F20C000.
//
// The guest configures channel 1 in FIFO mode and pushes 32-bit samples to
// the FIFO (or DAT1 while USEF1 is latched); the host drains them into a
// sample ring at a fixed per-slice rate (so the FIFO actually fills and the
// FULL1/EMPT1 handshake engages, like a real PWM draining at its own rate)
// and the browser plays the ring through WebAudio. The low 16 bits of each
// word are a signed 16-bit sample — the documented convention for this
// model (and what the pwm guest writes).
//
//   +0x00 CTL   PWEN1 bit0, MODE1 bit1, USEF1 bit5, CLRF1 bit6 (edge: clears
//               the FIFO), MSEN1 bit7; channel-2 bits ignored. The host
//               latches the level bits from guest writes and reflects the
//               canonical value back (write-mask semantics).
//   +0x04 STA   FULL1 bit0 / EMPT1 bit1, refreshed from the FIFO depth
//   +0x10 RNG1  range register (accepted, no effect in FIFO mode)
//   +0x14 DAT1  pushes a sample while USEF1 is latched
//   +0x20 FIFO  pushes a 32-bit sample (FIFO_DEPTH entries — simplification;
//               the real chip has 8)
//   +0x54 DONE  host extension: the guest writes 1 when the tune is done
//
// FIFO writes are observed with a range-limited HOOK_MEM_WRITE (unicorn
// callbacks carry the written value, so repeated samples can't be missed —
// a window diff could not tell two identical pushes apart). The hook is
// limited to [DAT1, FIFO+4) so the host's own STA/CTL refreshes (outside
// that range) never look like guest writes.

export function createPwm(uc, ucMod, base) {
  const CTL = base + 0x00;
  const STA = base + 0x04;
  const DAT1 = base + 0x14;
  const FIFO = base + 0x20;
  const DONE = base + 0x54;

  const CTL_LATCH = (1 << 0) | (1 << 1) | (1 << 5) | (1 << 7); // PWEN1|MODE1|USEF1|MSEN1
  const CLRF1 = 1 << 6;
  const FULL1 = 1 << 0;
  const EMPT1 = 1 << 1;
  // The guest can only observe FULL1 at slice boundaries (the window is
  // refreshed before each slice), so one slice's worth of pushes must fit
  // without overflow — depth 256 with a 64-sample per-slice drain.
  const FIFO_DEPTH = 256;
  const DRAIN_PER_SLICE = 64;

  const state = {
    base,
    ctl: 0, // latched level bits
    fifo: [], // undrained 32-bit samples
    ring: [], // drained samples (low 16 bits = signed sample)
    done: false,
    lastCtl: 0, // canonical CTL the host last wrote back
    drained: 0,
  };

  uc.hook_add(
    ucMod.HOOK_MEM_WRITE,
    (u, access, addr, size, value) => {
      const a = Number(addr);
      const v = Number(value) >>> 0;
      if (a === FIFO || (a === DAT1 && (state.ctl & (1 << 5)))) {
        if (state.fifo.length < FIFO_DEPTH) state.fifo.push(v);
      }
    },
    null,
    DAT1,
    FIFO + 4
  );

  function syncOut(uc) {
    let sta = 0;
    if (state.fifo.length >= FIFO_DEPTH) sta |= FULL1;
    if (state.fifo.length === 0) sta |= EMPT1;
    writeU32(uc, STA, sta);
    writeU32(uc, CTL, state.ctl);
  }

  function syncIn(uc) {
    const v = readU32(uc, CTL);
    if (v !== state.lastCtl) {
      state.lastCtl = v;
      state.ctl = v & CTL_LATCH;
      if (v & CLRF1) state.fifo.length = 0; // CLRF1 is an edge: clear the FIFO
    }
    if (readU32(uc, DONE) !== 0) state.done = true;
    const take = Math.min(state.fifo.length, DRAIN_PER_SLICE);
    for (let i = 0; i < take; i++) {
      state.ring.push(state.fifo.shift());
    }
    state.drained += take;
  }

  return { state, syncOut, syncIn };
}

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}

function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}
