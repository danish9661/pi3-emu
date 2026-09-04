export function parseElf(bytes) {
  if (bytes[0] !== 0x7f || bytes[1] !== 0x45 || bytes[2] !== 0x4c || bytes[3] !== 0x46) {
    throw new Error("not an ELF file");
  }
  if (bytes[4] !== 2) throw new Error("not a 64-bit ELF");
  if (bytes[5] !== 1) throw new Error("not little-endian ELF");
  const u16 = (o) => bytes[o] | (bytes[o + 1] << 8);
  const u64 = (o) => Number(
    bytes[o] +
    bytes[o + 1] * 0x100 +
    bytes[o + 2] * 0x10000 +
    bytes[o + 3] * 0x1000000 +
    bytes[o + 4] * 0x100000000 +
    bytes[o + 5] * 0x10000000000 +
    bytes[o + 6] * 0x1000000000000 +
    bytes[o + 7] * 0x100000000000000
  );
  const machine = u16(18);
  if (machine !== 183) throw new Error(`not AArch64 ELF (machine ${machine})`);
  const entry = u64(24);
  const phoff = u64(32);
  const phentsize = u16(54);
  const phnum = u16(56);
  const segments = [];
  for (let i = 0; i < phnum; i++) {
    const o = phoff + i * phentsize;
    const type = u32(bytes, o);
    if (type !== 1) continue;
    const p_offset = u64(o + 8);
    const p_vaddr = u64(o + 16);
    const p_filesz = u64(o + 32);
    const p_memsz = u64(o + 40);
    segments.push({
      vaddr: p_vaddr,
      bytes: bytes.slice(p_offset, p_offset + p_filesz),
      memsz: p_memsz,
    });
  }
  if (segments.length === 0) throw new Error("no PT_LOAD segments");
  return { entry, segments };
}

function u32(b, o) {
  return b[o] + b[o + 1] * 0x100 + b[o + 2] * 0x10000 + b[o + 3] * 0x1000000;
}

export function loadElf(uc, elf) {
  // unicorn.js mem_write is fragile: unaligned/odd-sized or overlapping
  // writes corrupt TCG state. Coalesce every segment into per-page buffers
  // and write whole pages (aligned address, aligned size).
  const PAGE = 4096;
  const pages = new Map();
  for (const seg of elf.segments) {
    let off = 0;
    while (off < seg.memsz) {
      const pageStart = seg.vaddr + off;
      const pageBase = pageStart & ~(PAGE - 1);
      const pageOff = pageStart - pageBase;
      let page = pages.get(pageBase);
      if (!page) {
        page = new Uint8Array(PAGE);
        pages.set(pageBase, page);
      }
      const n = Math.min(PAGE - pageOff, seg.memsz - off);
      page.set(seg.bytes.subarray(off, off + n), pageOff);
      off += n;
    }
  }
  for (const [base, page] of pages) {
    uc.mem_write(base, page);
  }
  return elf.entry;
}
