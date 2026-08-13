// Host-arbitrated BCM2835/2837 DMA controller.
//
// Real register layout, host-performed transfers: the DMA window
// (0x3F007000, channels 0..13 at 0x100 stride) is plain mapped RAM, so the
// host pulls CS/CONBLK_AD out after each slice and performs the control
// block chain between slices (the guest cannot observe a transfer in
// flight, only the END/INT results). Semantics implemented:
//
//   CS        ACTIVE(0)  guest sets 1 to start the chain (gated on the DMA
//                        ENABLE register, like the real chip); the host
//                        clears it when the chain finishes
//             END(1)     host sets when the chain completes
//             INT(2)     host sets on completion if the last CB had
//                        TI.INTEN (host extension, see below); the guest
//                        clears it by rewriting CS with the bit = 0
//             ABORT(31)  guest sets 1 to cancel: host clears all state
//   CONBLK_AD +0x04      guest: address of the first control block
//   TI        +0x08      SRC_INC(0) DEST_INC(1) SRC_IGNORE(6)
//                        DEST_IGNORE(7) INTEN(31, host extension: raise
//                        CS.INT at the end of the chain)
//   SRC_AD    +0x0C      source address (guest RAM)
//   DEST_AD   +0x10      destination address (guest RAM)
//   TXFR_LEN  +0x14      byte count (lower 20 bits, like the real register)
//   STRIDE    +0x18      not modeled (2D transfers ignored)
//   NEXTCONBK +0x1C      0 ends the chain; next CB otherwise
//
// Transfers copy bytes straight through guest RAM with page-aligned
// Uint8Array buffers (mem_write corrupts plain JS arrays). When the chain
// finishes the host latches END; the completion IRQ line is IC bit 16
// (DMA0) driven from CS.INT so it can flow through the M11 interrupt
// controller.

const PAGE = 4096;

export const DMA_CS_ACTIVE = 1;
export const DMA_CS_END = 2;
export const DMA_CS_INT = 4;

const TI_SRC_INC = 1;
const TI_DEST_INC = 2;
const TI_SRC_IGNORE = 1 << 6;
const TI_DEST_IGNORE = 1 << 7;
const TI_INTEN = 1 << 31;

function u32(b, off) {
  return b[off] + b[off + 1] * 0x100 + b[off + 2] * 0x10000 + b[off + 3] * 0x1000000;
}

// Perform one transfer, chunked so every mem_read/mem_write starts at a
// page boundary of both addresses with a Uint8Array buffer.
function transfer(uc, ti, src, dst, len) {
  const srcInc = ti & TI_SRC_INC;
  const dstInc = ti & TI_DEST_INC;
  const srcIgn = ti & TI_SRC_IGNORE;
  const dstIgn = ti & TI_DEST_IGNORE;
  const srcByte = srcIgn ? uc.mem_read(src, 1)[0] : 0;
  let s = src;
  let d = dst;
  let remaining = len;
  while (remaining > 0) {
    const chunk = Math.min(
      remaining,
      PAGE - (s & (PAGE - 1)),
      PAGE - (d & (PAGE - 1))
    );
    let buf;
    if (srcIgn) {
      buf = new Uint8Array(chunk).fill(srcByte);
    } else {
      buf = uc.mem_read(s, chunk);
      if (dstIgn) buf = new Uint8Array(chunk).fill(buf[chunk - 1]); // last byte wins
    }
    uc.mem_write(d, buf);
    if (srcInc) s += chunk;
    if (dstInc) d += chunk;
    remaining -= chunk;
  }
}

// Walk a control block chain in guest RAM, performing each transfer.
// Returns { int: last CB had TI.INTEN }.
export function dmaRunChain(uc, conblk) {
  let cb = conblk & ~0x1f;
  let inten = false;
  for (let n = 0; cb !== 0 && n < 64; n++) {
    const b = uc.mem_read(cb, 32);
    const ti = u32(b, 0);
    const src = u32(b, 4);
    const dst = u32(b, 8);
    const len = u32(b, 12) & 0xfffff;
    if (len !== 0) transfer(uc, ti, src, dst, len);
    if (ti & TI_INTEN) inten = true;
    cb = u32(b, 20);
  }
  return { int: inten };
}
