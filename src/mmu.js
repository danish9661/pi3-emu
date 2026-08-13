// Host-assisted MMU.
//
// This unicorn build cannot run a guest with the MMU enabled: `msr sctlr_el1`
// with M=1 raises UC_ERR_EXCEPTION and MAIR_EL1 is not implemented (both msr
// and mrs raise undefined-instruction exceptions). So translation is provided
// by the host. When the guest "enables" the MMU by writing root|1 to the
// MMU_CTL window (0x3F00D000), the host:
//
//   1. walks the guest's page tables (4K granule, 48-bit VA, standard block
//      and table descriptors) read out of guest RAM;
//   2. for every non-identity block, maps its VA range in the CPU with a
//      shadow copy of the PA contents (identity blocks need no shadow);
//   3. installs a UC_HOOK_MEM_WRITE hook that mirrors each guest write in
//      either direction, so the alias stays coherent and even code executing
//      from a shadow VA runs a live copy.
//
// Writes that miss every mapping (MMIO, unmapped VAs) are untouched; an
// access to a VA with no block mapping faults naturally as an unmapped
// access.

const PAGE = 4096;

function readDesc(uc, pa) {
  const b = uc.mem_read(pa, 8);
  return (
    b[0] +
    b[1] * 0x100 +
    b[2] * 0x10000 +
    b[3] * 0x1000000 +
    b[4] * 0x100000000 +
    b[5] * 0x10000000000
  );
}

// Walk a table page at tablePa covering VAs starting at vaBase; `shift` is
// the index shift (30 = L1, 21 = L2, 12 = L3). Collects block descriptors
// ({va, pa, size}); table descriptors descend one level.
function walkLevel(uc, tablePa, vaBase, shift, out) {
  for (let i = 0; i < 512; i++) {
    const d = readDesc(uc, tablePa + i * 8);
    const type = d & 3;
    if (type === 0) continue;
    const va = vaBase + i * (1 << shift);
    if (type === 1) {
      if (shift === 30) walkLevel(uc, d & ~(PAGE - 1), va, 21, out);
      else if (shift === 21) walkLevel(uc, d & ~(PAGE - 1), va, 12, out);
    } else {
      out.push({ va, pa: d & ~((1 << shift) - 1), size: 1 << shift });
    }
  }
}

export function mmuWalk(uc, rootPa) {
  const out = [];
  for (let i = 0; i < 512; i++) {
    const d = readDesc(uc, rootPa + i * 8);
    const type = d & 3;
    if (type === 0) continue;
    if (type === 1) walkLevel(uc, d & ~(PAGE - 1), i * 0x40000000, 30, out);
  }
  return out;
}

// Map every non-identity block at its VA with a shadow copy of the PA
// contents. Returns the enabled MMU state for mmuMirrorWrite.
export function mmuEnable(uc, ucMod, rootPa) {
  const entries = mmuWalk(uc, rootPa);
  const mappings = [];
  for (const e of entries) {
    if (e.va === e.pa) continue; // identity mapping: nothing to do
    try {
      uc.mem_map(e.va, e.size, ucMod.PROT_ALL);
    } catch {
      // already mapped
    }
    uc.mem_write(e.va, uc.mem_read(e.pa, e.size)); // page-aligned copy
    mappings.push(e);
  }
  return { root: rootPa, entries, mappings, enabled: true };
}

// Called from the UC_HOOK_MEM_WRITE hook: a write to either side of a
// non-identity mapping is mirrored to the other side.
export function mmuMirrorWrite(uc, state, addr, size, value) {
  if (!state || !state.enabled) return;
  for (const m of state.mappings) {
    if (addr >= m.va && addr < m.va + m.size) {
      mirror(uc, m.pa + (addr - m.va), size, value);
      return;
    }
    if (addr >= m.pa && addr < m.pa + m.size) {
      mirror(uc, m.va + (addr - m.pa), size, value);
      return;
    }
  }
}

function mirror(uc, target, size, value) {
  const bytes = new Uint8Array(size);
  let v = value < 0n ? BigInt.asUintN(64, value) : value;
  for (let i = 0; i < size; i++) {
    bytes[i] = Number(v & 0xffn);
    v >>= 8n;
  }
  uc.mem_write(target, bytes);
}
