// perf.js — Shared performance helpers for the device sync loop.
//
// Every readU32/writeU32 call allocates a new Uint8Array or plain array.
// With ~200 wasm crossings per slice, that's ~200 temporary allocations.
// This module reuses a single scratch buffer and provides batch helpers.

// Reusable scratch buffer for writeU32 (avoids per-call array allocation).
const _w32 = new Uint8Array(4);

// Read a little-endian U32 from guest memory (single wasm crossing).
export function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}

// Write a little-endian U32 to guest memory (single wasm crossing).
// Reuses a shared buffer — safe because wasm calls are sequential.
export function writeU32(uc, addr, v) {
  _w32[0] = v & 0xff;
  _w32[1] = (v >>> 8) & 0xff;
  _w32[2] = (v >>> 16) & 0xff;
  _w32[3] = (v >>> 24) & 0xff;
  uc.mem_write(addr, _w32);
}

// Read multiple U32s from contiguous addresses in fewer wasm crossings.
// addrs: array of guest addresses. Returns array of U32 values.
// Uses a single large mem_read when addresses are contiguous, falling
// back to individual reads otherwise.
export function readU32Batch(uc, addrs) {
  // Simple approach: read all in one shot if contiguous
  if (addrs.length === 0) return [];
  const min = Math.min(...addrs);
  const max = Math.max(...addrs) + 4;
  const range = max - min;
  // If addresses span more than 256 bytes, use individual reads
  if (range > 256) {
    return addrs.map((a) => readU32(uc, a));
  }
  const buf = uc.mem_read(min, range);
  return addrs.map((a) => {
    const off = a - min;
    return buf[off] | (buf[off + 1] << 8) | (buf[off + 2] << 16) | (buf[off + 3] << 24);
  });
}

// Write multiple U32s to contiguous addresses in fewer wasm crossings.
// pairs: array of [addr, value] pairs.
export function writeU32Batch(uc, pairs) {
  if (pairs.length === 0) return;
  const min = Math.min(...pairs.map((p) => p[0]));
  const max = Math.max(...pairs.map((p) => p[0])) + 4;
  const range = max - min;
  if (range > 256) {
    // Fallback: individual writes
    for (const [a, v] of pairs) writeU32(uc, a, v);
    return;
  }
  const buf = new Uint8Array(range);
  for (const [a, v] of pairs) {
    const off = a - min;
    buf[off] = v & 0xff;
    buf[off + 1] = (v >>> 8) & 0xff;
    buf[off + 2] = (v >>> 16) & 0xff;
    buf[off + 3] = (v >>> 24) & 0xff;
  }
  uc.mem_write(min, buf);
}
