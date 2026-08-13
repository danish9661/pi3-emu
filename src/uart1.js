// Host-arbitrated BCM2835 AUX mini UART (UART1) at 0x3F215000: a second
// console. The guest's writes to MU_IO (+0x40, byte) are pushed to the
// terminal tagged "[u1] " (one prefix per line) at write time: the host
// hooks MU_IO, so UART1 chars interleave with the primary UART's chars in
// the exact order the guest wrote them (a slice-diff would reorder chars
// written in the same slice). LSR bit 5 (TX empty) is refreshed every
// slice so the guest's TX pacing works. Output-only demo: input stays on
// the primary UART (there is no second terminal UI).
//
//   +0x04 AUX_ENABLES   bit 0 latched (mini UART enable)
//   +0x40 MU_IO         TX byte: hook-pushed to the console, then cleared
//   +0x54 LSR           host-refreshed: bit 5 TX empty, bit 6 TX idle
//   +0x60 CNTL, +0x4C LCR, +0x68 BAUD: latched, not modelled
//
// The guest's byte at +0x40 is cleared after every slice, so repeated
// identical characters are still detected (the window-diff pitfall).

export function createUart1(uc, ucMod, base, emit) {
  const IO = base + 0x40;
  const LSR = base + 0x54;
  const ENABLES = base + 0x04;

  const LSR_TX_EMPTY = 1 << 5;
  const LSR_TX_IDLE = 1 << 6;

  const state = {
    base,
    enabled: false,
  };

  // Fires only for the guest's MU_IO writes (host writes don't trigger
  // hooks in this unicorn build, so the pulse clear never re-emits).
  if (typeof emit === "function") {
    uc.hook_add(
      ucMod.HOOK_MEM_WRITE,
      (u, access, address, size, value) => {
        emit(Number(value) & 0xff);
      },
      null,
      IO,
      IO + 4
    );
  }

  function syncOut(uc) {
    // LSR: TX empty + TX idle so putc1's pacing loop never stalls.
    writeU32(uc, LSR, state.enabled ? LSR_TX_EMPTY | LSR_TX_IDLE : 0);
    writeU32(uc, ENABLES, state.enabled ? 1 : 0);
    // Pulse protocol: clear the TX byte the guest wrote (its putc1 waits
    // for the slot to clear, so one char flows per slice, losslessly).
    writeU32(uc, IO, 0);
  }

  function syncIn(uc) {
    if (readU32(uc, ENABLES) !== 0) state.enabled = true;
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
