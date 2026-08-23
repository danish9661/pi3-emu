// M23 linux-probe: boot the real arm64 kernel (6.1.182) on the emulator.
// The Image (text_offset=0) is loaded at the 2M-aligned base 0x200000 per
// the arm64 boot protocol (the kernel builds its early tables from the
// 2M-rounded phys of _text; the old 0x80000 load made the kernel's own
// mapping VA KIMAGE+X -> phys X while the code sat at phys 0x80000+X, i.e.
// everything ran 0x80000 off and the fork's fetch walk faulted at slice
// 4243). The stock bcm2837-rpi-3-b.dtb (memory node patched to 128 MB, a
// /chosen bootargs added for earlycon+console) goes at 0x3000000 (clear of
// the 36 MB image). The fork's fetch path truncates VAs to 32 bits and
// walks them through TTBR0 (the idmap), so the idmap L2 table is pre-seeded
// with alias entries mapping the truncated VAs 0x08000000..0x0a300000
// (kernel image) and 0x10000000 (KPTI trampoline) back to the image phys.
// The fork cannot fetch at pc 0 (TB-gen quirk), hence the 0x200000 base
// rather than 0. Sets x0=dtb x1..x3=0 (boot protocol, MMU off), ticks the
// arch timer at 19.2 MHz per slice and delivers IRQs natively via the local
// block. The console (PL011) is drained slice by slice; PASS = the earlycon
// banner + meminit progress.
// Usage: node test/linux-probe.mjs [maxSlices] [image] [dtb]
import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { createUart0 } = await import(join(__dirname, '..', 'src', 'uart0.js'));
const { createIc } = await import(join(__dirname, '..', 'src', 'ic.js'));
const { createLocalInt } = await import(join(__dirname, '..', 'src', 'localint.js'));
const { createSdhci } = await import(join(__dirname, '..', 'src', 'sdhci.js'));

const SLICE_INSNS = 4096;
const LINUX_RAM_SIZE = process.env.LINUX_RAM_SIZE ? Number(process.env.LINUX_RAM_SIZE) : 0x8000000;
// The arm64 boot protocol wants a 2M-aligned base (text_offset=0). The fork
// cannot fetch at pc 0 (a TB-gen quirk), so the image goes at 0x200000: the
// kernel's tables map VA KIMAGE+X -> phys 0x200000+X, and the fork's
// truncated fetches of KIMAGE VAs (0x08000000+X) resolve through the
// pre-seeded idmap aliases (seedIdmapAliases below).
const LINUX_IMAGE = 0x200000;
const LINUX_DTB = 0x3000000;

const [maxSlicesArg, imageArg, dtbArg] = process.argv.slice(2);
const MAX_SLICES = Number(maxSlicesArg) || 600000;
const KSRC = join(process.env.HOME || '/home/danish1075', 'linux-src', 'linux-6.1.182');
const IMAGE = imageArg || process.env.LINUX_IMAGE || join(KSRC, 'arch', 'arm64', 'boot', 'Image');
const DTB = dtbArg || process.env.LINUX_DTB || join(KSRC, 'arch', 'arm64', 'boot', 'dts', 'broadcom', 'bcm2837-rpi-3-b.dtb');

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}
function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}
// unicorn.js mem_write truncates large payloads (~16 MB); chunk the write.
function writeAll(uc, addr, bytes) {
  const CHUNK = 0x100000;
  for (let off = 0; off < bytes.length; off += CHUNK) {
    uc.mem_write(addr + off, Array.from(bytes.subarray(off, off + CHUNK)));
  }
}

// The fork's fetch path truncates VAs to 32 bits and walks them through
// TTBR0 (the idmap). The kernel's own create_idmap only fills the idmap L2
// entries for phys [0, 0x2866000) (blocks 0..0x14), so pre-seed the idmap
// L2 (phys 0x19e2000 = base 0x200000 + init_idmap_pg_dir image offset
// 0x17e0000 + 0x2000, per System.map) with:
//   [0x40..0x51] = 2M blocks mapping truncated KIMAGE VAs back to image phys
//   [0x80]       = a table pointer to an L3 page for the KPTI trampoline
// The L3 page is the reserved_pg_dir page @0x1908000 (the kernel loads it as
// the empty early ttbr1 and never writes it) with the trampoline's 3 pages
// (phys 0x1902000..0x1905000, not 2M-aligned) as 4K entries.
function seedIdmapAliases(uc, ucMod) {
  const BASE = 0x200000;
  const u64le = (v) => [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff, 0, 0, 0, 0];
  const idmapL2 = BASE + 0x17e2000;
  const blocks = [];
  for (let i = 0x40; i <= 0x51; i++) blocks.push(...u64le((BASE + ((i - 0x40) << 21)) | 0x701));
  uc.mem_write(idmapL2 + 0x40 * 8, blocks);
  uc.mem_write(idmapL2 + 0x80 * 8, u64le(BASE + 0x1708003));
  uc.mem_write(BASE + 0x1708000, [...u64le(BASE + 0x1702003), ...u64le(BASE + 0x1703003), ...u64le(BASE + 0x1704003)]);
}

// Patch the DTB's memory@0 reg size (offset of the <0 0x40000000> size cell)
// to 128 MB so the kernel only touches RAM we actually mapped.
// Real Pi firmware rewrites the DTB's VideoCore peripheral addresses
// (0x7e000000 region) to the ARM-physical view (0x3f000000 region) that
// the emulated memory map actually uses. Without this, the kernel ioremaps
// devices at 0x7e… which never lands on our 0x3f… device windows.
// Convert any 4-byte BE value in [0x7e000000,0x7effffff] -> [0x3f000000,0x3fffffff].
function patchDtbVcToArm(dtb) {
  const be = (o) => (dtb[o] << 24) | (dtb[o + 1] << 16) | (dtb[o + 2] << 8) | dtb[o + 3];
  let n = 0;
  for (let i = 0; i + 4 <= dtb.length; i++) {
    const v = be(i);
    if ((v & 0xff000000) === 0x7e000000) {
      dtb[i] = 0x3f;
      n++;
    }
  }
  if (n) console.log('patched', n, 'VC->ARM peripheral address(es)');
  return dtb;
}

function patchDtbRam(dtb) {
  // Walk the FDT structure block looking for the "memory@0" node and its
  // "reg" property. Minimal approach: scan for the 8-byte pattern
  // [0x00,0x00,0x00,0x00, 0x40,0x00,0x00,0x00] followed by nothing we care
  // about — the memory reg is the only <0x0 0x40000000> in the bcm2837 dtb.
  const pat = [0x40, 0x00, 0x00, 0x00];
  for (let i = 0; i + 8 <= dtb.length; i++) {
    if (dtb[i] === 0 && dtb[i + 1] === 0 && dtb[i + 2] === 0 && dtb[i + 3] === 0 &&
        dtb[i + 4] === pat[0] && dtb[i + 5] === pat[1] && dtb[i + 6] === pat[2] && dtb[i + 7] === pat[3]) {
      dtb[i + 4] = (LINUX_RAM_SIZE >> 24) & 0xff; // size cell high byte
      return true;
    }
  }
  return false;
}

// Insert a "bootargs" property into the /chosen node and rebuild the FDT
// (append the new string to the strings block). Returns the new buffer.
function patchDtbChosen(dtb) {
  const be32 = (o) => (((dtb[o] << 24) | (dtb[o + 1] << 16) | (dtb[o + 2] << 8) | dtb[o + 3]) >>> 0);
  const put32 = (b, o, v) => {
    b[o] = (v >>> 24) & 0xff; b[o + 1] = (v >>> 16) & 0xff; b[o + 2] = (v >>> 8) & 0xff; b[o + 3] = v & 0xff;
  };
  const offStruct = be32(8), offStrings = be32(12), sizeStruct = be32(36), sizeStrings = be32(32);
  const bootargs = 'earlycon=pl011,0x3f201000 console=ttyAMA0,115200 maxcpus=1 nr_cpus=1 mem=512M';
  const val = new Uint8Array(bootargs.length + 1);
  for (let i = 0; i < bootargs.length; i++) val[i] = bootargs.charCodeAt(i);
  const propPad = (4 - (val.length % 4)) % 4;
  // find the /chosen node's property list (before its END_NODE token)
  let o = offStruct, chosenEnd = -1;
  const stack = [];
  while (o < offStruct + sizeStruct) {
    const t = be32(o);
    if (t === 1) {
      let i = o + 4; while (dtb[i] !== 0) i++;
      stack.push(String.fromCharCode(...dtb.subarray(o + 4, i)));
      o = i + 1 + ((4 - ((i - o - 4 + 1) % 4)) % 4);
    } else if (t === 2) {
      if (stack.length && stack[stack.length - 1] === 'chosen') chosenEnd = o;
      stack.pop();
      o += 4;
    } else if (t === 3) {
      const len = be32(o + 4);
      o += 12 + len + ((4 - (len % 4)) % 4);
    } else break;
  }
  if (chosenEnd < 0) {
    console.log('WARN: /chosen node not found; no bootargs added');
    return dtb;
  }
  const strOff = sizeStrings;
  const prop = new Uint8Array(12 + val.length + propPad);
  put32(prop, 0, 3); // FDT_PROP
  put32(prop, 4, val.length);
  put32(prop, 8, strOff);
  prop.set(val, 12);
  const nbuf = new Uint8Array(dtb.length + prop.length + 9);
  nbuf.set(dtb.subarray(0, chosenEnd));
  nbuf.set(prop, chosenEnd);
  nbuf.set(dtb.subarray(chosenEnd, offStrings), chosenEnd + prop.length);
  nbuf.set(dtb.subarray(offStrings, offStrings + sizeStrings), offStrings + prop.length);
  for (let i = 0; i < 8; i++) nbuf[offStrings + prop.length + sizeStrings + i] = 'bootargs\0'.charCodeAt(i);
  nbuf[offStrings + prop.length + sizeStrings + 8] = 0;
  nbuf[0] = 0xd0; nbuf[1] = 0x0d; nbuf[2] = 0xfe; nbuf[3] = 0xed;
  put32(nbuf, 4, nbuf.length); // totalsize
  put32(nbuf, 8, offStruct); // off_dt_struct (unchanged)
  put32(nbuf, 12, offStrings + prop.length); // off_dt_strings (shifted)
  put32(nbuf, 32, sizeStrings + 9); // size_dt_strings
  put32(nbuf, 36, sizeStruct + prop.length); // size_dt_struct
  return nbuf;
}

// Map every peripheral address outside the modeled windows as plain RAM
// (mirror of main.js mapBlackHole).
function mapBlackHole(uc, ucMod, lo, hi, skip) {
  const runs = skip.map(([b, s]) => [b, b + s]).sort((a, b) => a[0] - b[0]);
  let cur = lo;
  for (const [b, e] of runs) {
    if (b > cur) uc.mem_map(cur, b - cur, ucMod.PROT_READ | ucMod.PROT_WRITE);
    if (e > cur) cur = e;
  }
  if (hi > cur) uc.mem_map(cur, hi - cur, ucMod.PROT_READ | ucMod.PROT_WRITE);
}

async function main() {
  const ucMod = await MUnicorn();
  const board = (
    await WebAssembly.instantiate(
      readFileSync(join(__dirname, '..', 'public', 'pi_board.wasm')),
      {}
    )
  ).instance.exports;
  const uart = Number(board.pi_uart_base());
  console.error('UART base = 0x' + uart.toString(16));
  const image = new Uint8Array(readFileSync(IMAGE));
  const dtbRaw = new Uint8Array(readFileSync(DTB));
  const dtb = new Uint8Array(dtbRaw);
  if (dtb[0] !== 0xd0 || dtb[1] !== 0x0d || dtb[2] !== 0xfe || dtb[3] !== 0xed) {
    throw new Error('bad DTB magic in ' + DTB);
  }
  if (!patchDtbRam(dtb)) console.log('WARN: memory reg patch pattern not found');

  const textOffset = Number(
    BigInt(image[8] | (image[9] << 8) | (image[10] << 16) | (image[11] << 24)) |
      ((BigInt(image[12]) | (BigInt(image[13]) << 8n) | (BigInt(image[14]) << 16n) | (BigInt(image[15]) << 24n)) << 32n)
  );
  const entry = LINUX_IMAGE + textOffset;
  console.log('Image:', IMAGE.split('/').pop(), image.length, 'bytes | entry 0x' + entry.toString(16), '| dtb', dtb.length, 'bytes');

  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);

  // Test: use cortex-a72 instead of A53 to see if the snprintf loop is model-specific.
  if (typeof uc.ctl_set_cpu_model === 'function') {
    const A72 = 2; // UC_CPU_ARM64_A72
    try { uc.ctl_set_cpu_model(A72); console.log('[cpu] ctl_set_cpu_model(A72) called'); }
    catch (e) { console.log('[cpu] ctl_set_cpu_model failed:', String(e).slice(0, 100)); }
    try { console.log('[cpu] model now =', uc.ctl_get_cpu_model && uc.ctl_get_cpu_model()); } catch(e) { console.log('[cpu] ctl_get_cpu_model err', e.message); }
  } else { console.log('[cpu] no ctl_set_cpu_model available'); }

  const DELIVER = process.env.DELIVER !== '0';
  if (DELIVER && typeof uc.arm64_deliver_exceptions === 'function') {
    uc.arm64_deliver_exceptions(1);   // let the kernel run its own exception handlers
  }
  const MARK = (s) => { console.error('MARK', s); try { uc.reg_read_i32(ucMod.ARM64_REG_PC); } catch (e) { console.error('  RDERR', e.message); } };
  uc.mem_map(0, LINUX_RAM_SIZE, ucMod.PROT_ALL);
  MARK('after mem_map RAM');
  // Only the windows modeled below are excluded from the black hole; every
  // other peripheral address stays zero-filled RAM.
  mapBlackHole(uc, ucMod, 0x3f000000, 0x3fa00000, [
    [0x3f00b000, 0x1000], [uart, 0x1000], [0x3f300000, 0x1000],
  ]);
  uc.mem_map(0x3f00b000, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(uart, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(0x3f300000, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(0x40000000, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE);
  MARK('after device maps');

  // Apply R_AARCH64_RELATIVE relocations to the loaded Image.
  // The fork does not correctly execute the kernel's self-relocation loop in
  // head.S (__relocate_kernel), so absolute data pointers (e.g.
  // console_sem.wait_list, init_task self-refs) stay 0 and the kernel oopses
  // at the first printk. We apply them here with delta=0 (kernel loaded at its
  // link base), exactly what __relocate_kernel does when loaded in place.
  {
    const KIMAGE = 0xffff800008000000n;
    const relaStart = 0x1896260; // __rela_start VA 0xffff800009896260 - KIMAGE
    const relaEnd = 0x1e198c8;   // __rela_end
    const rd64 = (off) => { let v = 0n; for (let i = 7; i >= 0; i--) v = (v << 8n) | BigInt(image[off + i]); return v; };
    const wr64 = (off, val) => { const v = BigInt.asUintN(64, val); for (let i = 0; i < 8; i++) image[off + i] = Number((v >> BigInt(i * 8)) & 0xffn); };
    let n = 0;
    for (let o = relaStart; o + 24 <= relaEnd; o += 24) {
      const r_offset = rd64(o);
      const r_info = rd64(o + 8);
      const r_addend = rd64(o + 16);
      if ((r_info & 0xffffffn) === 1027n) { // R_AARCH64_RELATIVE
        const loc = Number(BigInt.asUintN(64, r_offset - KIMAGE));
        if (loc >= 0 && loc + 8 <= image.length) { wr64(loc, r_addend); n++; }
      }
    }
    console.log('[reloc] applied', n, 'R_AARCH64_RELATIVE relocations to Image');
  }
  writeAll(uc, LINUX_IMAGE, image);
  // Skip the kernel's self-relocation loop: __relocate_kernel is a redundant
  // no-op here because we already applied the R_AARCH64_RELATIVE relocations
  // host-side (delta=0), which matches the kernel's x23=0 identity relocations
  // (head.S sets x23 = KERNEL_START & MIN_KIMG_ALIGN-1 | kaslr; observed 0).
  // Patching its entry to 'ret' saves ~150k iterations through the fork's slow
  // MMU-on page walks and lets early boot proceed immediately.
  const RELD_PC_VA = 0xffff800008e46438n; // __relocate_kernel (System.map)
  const reldPa = Number(RELD_PC_VA - 0xffff800008000000n) + LINUX_IMAGE;
  uc.mem_write(reldPa, Buffer.from([0xc0, 0x03, 0x5f, 0xd6])); // ret (0xD65F03C0)
  console.log('[reloc] skipped __relocate_kernel (entry -> ret) at pa 0x' + reldPa.toString(16));
  MARK('after writeAll Image');
  // The fork's fetch path truncates VAs to 32 bits and walks them through
  // TTBR0 (the idmap). Seed the idmap L2 (init_idmap_pg_dir = phys 0x17e0000
  // with base=0; L1 @0x17e1000, L2 @0x17e2000 — System.map) with alias
  // entries so truncated kernel-image fetches 0x08000000..0x0a300000 resolve
  // to image phys (VA KIMAGE+X -> phys X for the 2M-aligned base) and the
  // KPTI trampoline alias 0x10000000 -> phys 0x1702000 via a 4K L3 table in
  // the (never written by the kernel) reserved_pg_dir page @0x1708000.
  seedIdmapAliases(uc, ucMod);
  MARK('after seedIdmapAliases');
  writeAll(uc, LINUX_DTB, patchDtbChosen(dtb));
  MARK('after writeAll DTB');
  uc.entry = entry;
  uc.reg_write_i64(ucMod.ARM64_REG_SP, BigInt(LINUX_RAM_SIZE - 0x10000));
  uc.reg_write_i64(ucMod.ARM64_REG_X0, BigInt(LINUX_DTB));
  uc.reg_write_i64(ucMod.ARM64_REG_X1, 0n);
  uc.reg_write_i64(ucMod.ARM64_REG_X2, 0n);
  uc.reg_write_i64(ucMod.ARM64_REG_X3, 0n);

  let emitted = 0;
  const txBuf = [];
  const uart0 = createUart0(uc, ucMod, uart, (b) => {
    if (emitted < 256) { process.stderr.write('TX<' + String.fromCharCode(b)); emitted++; }
    txBuf.push(b);
    board.pi_cons_push(b);
  });
   let uartWrites = 0;
   let lastMem = { addr: 0, size: 0, type: '?' };
    uc.hook_add(ucMod.HOOK_MEM_WRITE, (u, access, addr, size, value) => {
     lastMem = { addr: Number(addr), size: Number(size), type: 'W' };
     if (uartWrites < 16) { process.stderr.write(`UARTWR @0x${Number(addr).toString(16)} sz${Number(size)} v0x${Number(value).toString(16)}\n`); uartWrites++; }
   }, null, uart, uart + 0x1000);
  const ic = createIc(uc, ucMod, 0x3f00b200, () => ({
    timer: 0, dma0: false, pl011: uart0.irqActive(), sdhci: false,
    gpio0: false, gpio1: false, aux: false,
  }));
  const localInt = createLocalInt(uc, ucMod, 0x40000000, () => ({
    cntps: Number(uc.arm64_debug(13)) ? 1 : 0,
    cntpns: Number(uc.arm64_debug(3)) ? 1 : 0,
    cnthp: Number(uc.arm64_debug(12)) ? 1 : 0,
    cntv: Number(uc.arm64_debug(11)) ? 1 : 0,
    gpu: ic.line() ? 1 : 0,
    pmu: 0, axi: 0, ltimer: 0,
    mailbox: [0, 0, 0, 0],
  }));
  createSdhci(uc, ucMod, 0x3f300000, () => {});

  const tmrWall0 = performance.now();
  let chars = '';
  let steps = 0;
  // A ring of the last instructions before the exception, so the path into
  // the fault is visible (register reads are unreliable in this build).
    // (per-instruction HOOK_CODE tracing removed: it fired a JS callback for
    // EVERY instruction and made the boot ~100x too slow. The up()/__up()/
    // wake_q_add path was already verified correct via narrow single-PC
    // hooks: &console_sem arrives 64-bit in up(), list_empty reads correctly,
    // and the empty-list path returns without calling __up.)


   const VSNPRINTF = BigInt('0xffff800008e0ea00');
  let vsnHits = 0;
  // Generic AArch64 page-table walk: root = PA of level-0 table, shifts[] = index bit positions per level.
  const readU64 = (pa) => { const b = uc.mem_read(Number(BigInt.asUintN(64, pa) & 0xffffffffn), 8); let v = 0n; for (let i = 7; i >= 0; i--) v = (v << 8n) | BigInt(b[i]); return v; };
  const walkVA = (vaBig, root, shifts) => {
    const va = BigInt.asUintN(64, vaBig);
    let table = root & 0x0000FFFFFFFFF000n;
    for (const sh of shifts) {
      const idx = (va >> sh) & 0x1ffn;
      const desc = readU64(table + idx * 8n);
      const type = desc & 3n;
      if (type === 3n) { table = desc & 0x0000FFFFFFFFF000n; continue; }
      if (type === 1n) { const mask = (1n << sh) - 1n; return (desc & 0x0000FFFFFFFFF000n) | (va & mask); }
      return null;
    }
    return null;
  };
  const readStr = (pa, n=32) => {
    const b = uc.mem_read(Number(BigInt.asUintN(64, pa) & 0xffffffffn), n);
    let s=''; for (const c of b) { if (c===0) break; if (c>=32&&c<127) s+=String.fromCharCode(c); else s+='\\x'+c.toString(16).padStart(2,'0'); }
    return s;
  };
   // (VSNPRINTF HOOK_CODE removed: also fired a JS callback per instruction.)

  let unmappedHits = 0;
  const unmappedSeen = {};
  for (const [kind, type] of [
    ['FETCH', ucMod.HOOK_MEM_FETCH_UNMAPPED],
    ['READ', ucMod.HOOK_MEM_READ_UNMAPPED],
    ['WRITE', ucMod.HOOK_MEM_WRITE_UNMAPPED],
  ]) {
    uc.hook_add(type, (u, access, addr, size, value) => {
      const a = Number(addr);
      // Cap logging: report each distinct address a few times, then stop, so a
      // tight spin on an unmapped MMIO reg doesn't flood the log / slow the run.
      const key = kind + '@' + a.toString(16);
      const seen = unmappedSeen[key] || 0;
      if (seen < 3 && unmappedHits < 60) {
        unmappedSeen[key] = seen + 1;
        unmappedHits++;
        let pc = 0;
        try { pc = Number(uc.arm64_debug(5)); } catch (e) {}
        const val = (type === ucMod.HOOK_MEM_WRITE_UNMAPPED) ? ' v=0x' + Number(value).toString(16) : '';
        console.log(`UNMAPPED ${kind} @ 0x${a.toString(16)} pc 0x${pc.toString(16)} size ${Number(size)}${val}`);
      }
    });
  }
  // Trace where the kernel writes the idmap page tables (PA 0x19e0000..0x19e3000).
  uc.hook_add(ucMod.HOOK_MEM_WRITE, (u, access, addr, size, value) => {
    const a = Number(addr);
    if (a >= 0x19e0000 && a < 0x19e3000) {
      console.log(`IDMAP-PA-WRITE @0x${a.toString(16)} = 0x${Number(value).toString(16)} size ${Number(size)}`);
    }
  }, 0x19e0000, 0x19e3000);
  const drain = () => {
    let out = '';
    for (;;) {
      const ch = Number(board.pi_cons_poll());
      if (ch === -1 || ch === 0xffffffff) break;
      out += String.fromCharCode(ch);
    }
    while (txBuf.length) out += String.fromCharCode(txBuf.shift());
    return out;
  };
  let gTtbr1 = 0n, gTcr = 0n, gTtbr0 = 0n;
  let aborted = false;
  const blockRing = [];
  try {
    uc.hook_add(ucMod.HOOK_BLOCK, (u, addr, size) => {
      blockRing.push(BigInt(addr));
      if (blockRing.length > 24) blockRing.shift();
    });
    console.log('HOOK_BLOCK installed');
  } catch (e) { console.log('HOOK_BLOCK err', e.message); }
  const slice = () => {
    let lastPc = 0n;
    // reg_read_i32(ARM64_REG_PC) is deprecated (id 0) in this fork and returns 0,
    // which would force emu_start to restart at the physical entry every slice and
    // trap the kernel in head.S. Use arm64_debug(5) (env.pc) for the real PC.
    let pc = uc.arm64_debug(5);
    if (!pc) pc = BigInt(entry);
    lastPc = pc;
    // Capture the kernel page-table roots before emu_start (wasm state valid here).
    try { gTtbr1 = BigInt.asUintN(64, uc.reg_read_i64(ucMod.ARM64_REG_TTBR1_EL1)); } catch (_) {}
    try { gTtbr0 = BigInt.asUintN(64, uc.reg_read_i64(ucMod.ARM64_REG_TTBR0_EL1)); } catch (_) {}
    try { gTcr = BigInt.asUintN(64, uc.reg_read_i64(ucMod.ARM64_REG_TCR_EL1)); } catch (_) {}
    try { localInt.syncOut(uc); } catch (e) { console.log('SYNCOUT localInt THREW:', String(e).slice(0,120)); throw e; }
    try { ic.syncOut(uc); } catch (e) { console.log('SYNCOUT ic THREW:', String(e).slice(0,120)); throw e; }
    try { uart0.syncOut(uc); } catch (e) { console.log('SYNCOUT uart0 THREW:', String(e).slice(0,120)); throw e; }
    // arch timer at the real 19.2 MHz rate
    try { uc.arm64_timer_tick(BigInt(Math.floor((performance.now() - tmrWall0) * 1000 * 19.2))); } catch (e) { console.log('TIMER_TICK THREW:', String(e).slice(0,120)); throw e; }
    try {
      const insns = SLICE_INSNS;
      uc.emu_start(pc, 0, 0, insns);
    } catch (e) {
      console.log('EXC-INNER at slice', steps, ': ', String(e && e.message || e).slice(0, 200));
      aborted = true;
      if (e && e.stack) console.log('STACK:\n' + String(e.stack).slice(0, 2500));
      let faultPc = 0n;
      try { faultPc = BigInt(uc.arm64_debug(5)); } catch (_) { console.log('  arm64_debug(5) failed'); }
      console.log('  fault pc (full 64b)=0x' + faultPc.toString(16) + ' | lastPc(before emu_start)=0x' + (lastPc||0).toString(16));
      console.log('  begin PC (emu_start arg)=0x' + pc.toString(16));
      console.log('  BLOCK RING (last executed blocks): ' + blockRing.map(x => x.toString(16)).join(' '));
      try {
        const names = ['x0','x1','x2','x3','x4','x5','x6','x7','x8','x9','x10','x11','x12','x13','x14','x15','x16','x17','x18','x19','x20','x21','x22','x23','x24','x25','x26','x27','x28','x29','x30','sp'];
        let line = '';
        for (const nm of names) {
          const id = ucMod['ARM64_REG_' + nm.toUpperCase()];
          if (id === undefined) { line += ` ${nm}=?`; continue; }
          const b = uc.reg_read(id, 8); let v = 0n; for (let j = 7; j >= 0; j--) v = (v << 8n) | BigInt(b[j]);
          line += ` ${nm}=0x${BigInt.asUintN(64, v).toString(16)}`;
        }
        console.log('  REGS AT FAULT:' + line + ' pc=0x' + faultPc.toString(16));
        try {
          const b = uc.reg_read(ucMod.ARM64_REG_SP_EL0, 8); let v = 0n; for (let j = 7; j >= 0; j--) v = (v << 8n) | BigInt(b[j]);
          console.log('  SP_EL0 AT FAULT = 0x' + BigInt.asUintN(64, v).toString(16));
        } catch (e3) { console.log('  SP_EL0 read err: ' + e3.message); }
      } catch (e2) { console.log('  REGS dump err: ' + e2.message); }
      try { console.log('  EXC-INDEX=' + Number(uc.arm64_debug(6)) + ' ESR_EL1=0x' + Number(uc.arm64_debug(7)).toString(16) + ' FAR_EL1=0x' + Number(uc.arm64_debug(70)).toString(16) + ' SCTLR_EL1=0x' + Number(uc.arm64_debug(71)).toString(16) + ' HCR_EL2=0x' + Number(uc.arm64_debug(72)).toString(16)); } catch (e4) {}
      try {
        // tcg_abort now records its site in a fork global read via
        // arm64_debug(90)=magic, arm64_debug(91)=caller RA.
        const m0 = Number(uc.arm64_debug(90));
        const m1 = Number(uc.arm64_debug(91));
        console.log('  TCG_ABORT MARKER: magic=0x' + m0.toString(16) + ' RA=0x' + m1.toString(16));
        try { const tpc = Number(uc.arm64_debug(92)); console.log('  TCG_ABORT translated PC (g_tcg_pc)=0x' + (tpc>>>0).toString(16) + ' / full=' + tpc.toString(16)); } catch (e6) { console.log('  tcg_pc read err: ' + e6.message); }
      } catch (e5) { console.log('  marker read err: ' + e5.message); }
      console.log('  LAST MEM ACCESS before abort: ' + lastMem.type + ' addr=0x' + lastMem.addr.toString(16) + ' size=' + lastMem.size);
       console.log('  TRACE tail: (per-instruction trace disabled for performance)');
      // Page-walk the faulting fetch VA using the REAL TCR (arm64_debug(60)
      // returns env->cp15.tcr_el[1].raw_tcr; the captured gTcr reg_read fails
      // and returns 0, which corrupts the walk). Kernel uses TTBR1 for high
      // VAs like 0xffff8000...
      try {
         const faultVaFull = faultPc;   // now accurate (env->pc synced per-instruction)
        const faultVa32 = faultVaFull & 0xffffffffn; // fork may truncate to 32b
        const tryWalk = (label, faultVa) => {
        const readU64 = (pa) => {
          const b = uc.mem_read(Number(pa), 8);
          let v = 0n; for (let i = 7; i >= 0; i--) v = (v << 8n) | BigInt(b[i]);
          return v;
        };
        const tcr = BigInt(Number(uc.arm64_debug(60)));
        const t1sz = Number((tcr >> 16n) & 0x3fn);
        const tg1 = Number((tcr >> 30n) & 0x3n);
        const gran = (tg1 === 2) ? 12 : (tg1 === 1 ? 16 : 14); // TG1: 0b10=4K 0b01=64K 0b11=16K
        const ttbr1 = gTtbr1;
        const base0 = (ttbr1 >> 12n) << 12n;
        const inputsize = 64 - t1sz;
        const numLevels = Math.ceil((inputsize - gran) / 9);
        const startLevel = 4 - numLevels;
        console.log('  [' + label + '] walk: ttbr1=0x' + ttbr1.toString(16) + ' tcr=0x' + tcr.toString(16) +
                    ' T1SZ=' + t1sz + ' TG1=' + tg1 + ' gran=' + gran +
                    ' inputsize=' + inputsize + ' numLevels=' + numLevels + ' startLevel=' + startLevel);
        let table = base0; let pa = 0n; let ok = false; let lastLvl = -1;
        for (let lvl = startLevel; lvl <= 3; lvl++) {
          const shift = inputsize - 9 * (1 + (lvl - startLevel));
          const idx = (faultVa >> BigInt(shift)) & 0x1ffn;
          const ent = readU64(table + idx * 8n);
          const type = Number(ent & 0x3n);
          console.log('   L' + lvl + ' shift=' + shift + ' idx=' + idx + ' ent=0x' + ent.toString(16) + ' type=' + type);
          if (type === 0) break;            // invalid
          if (type === 1) {                 // block / page
            const mask = (1n << BigInt(shift)) - 1n;
            pa = (ent & ~mask & 0x0000FFFFFFFFF000n) | (faultVa & mask);
            ok = true; lastLvl = lvl; break;
          }
          if (type === 2 || type === 3) {   // table descriptor (this fork uses type 3)
            table = (ent >> 12n) << 12n;
            continue;
          }
          break;
        }
        if (ok) {
          console.log('  [' + label + '] -> fault PA = 0x' + pa.toString(16) + ' (L' + lastLvl + ')');
          const b = uc.mem_read(Number(pa), 16);
          const hex = Array.from(b).map((x) => x.toString(16).padStart(2, '0')).join('');
          console.log('  [' + label + '] insn bytes: ' + hex);
          const words = [];
          for (let i = 0; i < 16; i += 4) {
            const w = (b[i] | (b[i+1]<<8) | (b[i+2]<<16) | (b[i+3]<<24)) >>> 0;
            words.push('0x' + w.toString(16));
          }
          console.log('  [' + label + '] insn words: ' + words.join(' '));
        } else { console.log('  [' + label + '] walk failed (invalid descriptor)'); }
        };
        try { tryWalk('full-va', faultVaFull); } catch (e2) { console.log('  full-va walk failed:', String(e2).slice(0,120)); }
        try { tryWalk('trunc-32', faultVa32); } catch (e2) { console.log('  trunc-32 walk failed:', String(e2).slice(0,120)); }
        return; // (skip the rest of the stale diagnostics)
        const readU64 = (pa) => {
          const b = uc.mem_read(Number(pa), 8);
          let v = 0n; for (let i = 7; i >= 0; i--) v = (v << 8n) | BigInt(b[i]);
          return v;
        };
        const tcr = BigInt(Number(uc.arm64_debug(60)));
        const t1sz = Number((tcr >> 16n) & 0x3fn);
        const tg1 = Number((tcr >> 30n) & 0x3n);
        const gran = (tg1 === 2) ? 12 : (tg1 === 1 ? 16 : 14); // TG1: 0b10=4K 0b01=64K 0b11=16K
        const ttbr1 = gTtbr1;
        const base0 = (ttbr1 >> 12n) << 12n;
        const inputsize = 64 - t1sz;
        const numLevels = Math.ceil((inputsize - gran) / 9);
        const startLevel = 4 - numLevels;
        console.log('  walk: ttbr1=0x' + ttbr1.toString(16) + ' tcr=0x' + tcr.toString(16) +
                    ' T1SZ=' + t1sz + ' TG1=' + tg1 + ' gran=' + gran +
                    ' inputsize=' + inputsize + ' numLevels=' + numLevels + ' startLevel=' + startLevel);
        let table = base0; let pa = 0n; let ok = false; let lastLvl = -1;
        for (let lvl = startLevel; lvl <= 3; lvl++) {
          const shift = inputsize - 9 * (1 + (lvl - startLevel));
          const idx = (faultVa >> BigInt(shift)) & 0x1ffn;
          const ent = readU64(table + idx * 8n);
          const type = Number(ent & 0x3n);
          console.log('   L' + lvl + ' shift=' + shift + ' idx=' + idx + ' ent=0x' + ent.toString(16) + ' type=' + type);
          if (type === 0) break;            // invalid
          if (type === 1) {                 // block / page
            const mask = (1n << BigInt(shift)) - 1n;
            pa = (ent & ~mask & 0x0000FFFFFFFFF000n) | (faultVa & mask);
            ok = true; lastLvl = lvl; break;
          }
          if (type === 2 || type === 3) {   // table descriptor (this fork uses type 3)
            table = (ent >> 12n) << 12n;
            continue;
          }
          break;
        }
        if (ok) {
          console.log('  -> fault PA = 0x' + pa.toString(16) + ' (L' + lastLvl + ')');
          const b = uc.mem_read(Number(pa), 16);
          const hex = Array.from(b).map((x) => x.toString(16).padStart(2, '0')).join('');
          console.log('  insn bytes: ' + hex);
          const words = [];
          for (let i = 0; i < 16; i += 4) {
            const w = (b[i] | (b[i+1]<<8) | (b[i+2]<<16) | (b[i+3]<<24)) >>> 0;
            words.push('0x' + w.toString(16));
          }
          console.log('  insn words: ' + words.join(' '));
        } else { console.log('  walk failed (invalid descriptor)'); }
      } catch (e2) { console.log('  walk/insn read failed:', String(e2).slice(0, 120)); }
      throw e;
      // Walk the CRASH VA through BOTH TTBRs to decide fork-MMU-bug vs kernel-layout.
      try {
        const rdU64b = (reg) => { const b = uc.reg_read(reg, 8); let v = 0n; for (let i = 7; i >= 0; i--) v = (v << 8n) | BigInt(b[i]); return v; };
        const crashVa = BigInt(Number(uc.arm64_debug(5)) >>> 0);
        const tcr1 = Number(uc.arm64_debug(60));
        const walkTTBR = (label, ttbrReg, nsz, va) => {
          const TTBR = rdU64b(ttbrReg);
          console.log(`  [${label}] TTBR=0x${TTBR.toString(16)} TNSZ=${nsz} va=0x${va.toString(16)}`);
          let base = TTBR & 0x0000FFFFFFFFF000n;
          for (let lvl = 0; lvl < 4; lvl++) {
            const sh = 9 * (4 - lvl);
            const idx = Number((va >> BigInt(sh)) & 0x1ffn);
            const descAddr = base + BigInt(idx * 8);
            let desc = 0n;
            try { const b = uc.mem_read(descAddr, 8); for (let j = 7; j >= 0; j--) desc = (desc << 8n) | BigInt(b[j]); }
            catch (e) { console.log(`    L${lvl} OOB @0x${descAddr.toString(16)} idx ${idx} (base 0x${base.toString(16)})`); break; }
            const type = Number(desc & 3n);
            const pa = desc & 0x0000FFFFFFFFF000n;
            console.log(`    L${lvl} [${idx}] @0x${descAddr.toString(16)} desc 0x${desc.toString(16)} type ${type} pa 0x${pa.toString(16)}`);
            if (type === 3) { base = pa; continue; }
            if (type === 1) { const off = va & ((1n << BigInt(9 * (4 - lvl))) - 1n); console.log(`    L${lvl} BLOCK -> pa 0x${(pa + off).toString(16)}`); break; }
            console.log(`    L${lvl} INVALID`); break;
          }
        };
        walkTTBR('TTBR0/crashVA', ucMod.ARM64_REG_TTBR0_EL1, tcr1 & 0x3f, crashVa);
        walkTTBR('TTBR1/crashVA', ucMod.ARM64_REG_TTBR1_EL1, (tcr1 >> 16) & 0x3f, crashVa);
      } catch (e) { console.log('  crash-va walk failed:', String(e).slice(0, 120)); }
      // Decode the LAST executed instruction + branch-target registers.
      try {
        const lastPc = 0x9723540n;
        const b = uc.mem_read(Number(lastPc), 4);
        console.log('  last-insn @0x' + lastPc.toString(16) + ': ' + Array.from(b).map((x) => x.toString(16).padStart(2, '0')).join(''));
      } catch (e) { console.log('  last-insn read failed:', String(e).slice(0, 80)); }
      try {
        const Hr = (r) => BigInt.asUintN(64, uc.reg_read_i64(r));
        for (const [n, r] of [['x9', ucMod.ARM64_REG_X9], ['x10', ucMod.ARM64_REG_X10], ['x11', ucMod.ARM64_REG_X11], ['x12', ucMod.ARM64_REG_X12], ['x13', ucMod.ARM64_REG_X13], ['x15', ucMod.ARM64_REG_X15], ['x16', ucMod.ARM64_REG_X16], ['x17', ucMod.ARM64_REG_X17], ['x18', ucMod.ARM64_REG_X18], ['x19', ucMod.ARM64_REG_X19], ['x20', ucMod.ARM64_REG_X20], ['x21', ucMod.ARM64_REG_X21], ['x22', ucMod.ARM64_REG_X22], ['x30/lr', ucMod.ARM64_REG_LR]]) {
          console.log('    ' + n + ' = 0x' + Hr(r).toString(16));
        }
      } catch (e) { console.log('  reg dump failed:', String(e).slice(0, 80)); }
      console.log('trace:', trace.join(' '));
      // DECISIVE: force ONE walk at a time and dump the fork's own walk
      // results + the bytes it resolved to, for each candidate VA form.
      // If mem_read(X) returns the loop bytes (3f010aeb...) the walk mapped
      // X -> phys 0xec645c; a throw = translation fault in the fork walk.
      const walkDump = (label) => {
        const mk = (s) => Number(uc.arm64_debug(s)).toString(16);
        const ring = [];
        for (let i = 0; i < 8; i++) ring.push('[' + mk(31 + i) + '/' + mk(39 + i) + ']');
        console.log('  walk', label, ': ret', mk(14), 'fault', mk(15), 'ttbr', mk(16),
          'tcr', mk(17), 'faultva', mk(22), 'ttbr_used', mk(24), 'level', mk(25),
          'inputsize', mk(26), 'ptw_reads', mk(30), '| ring:', ring.join(' '));
      };
      const tryRead = (label, va) => {
        try {
          const b = uc.mem_read(va, 8);
          console.log('READ', label, '0x' + va.toString(16), '->',
            Array.from(b).map((x) => x.toString(16).padStart(2, '0')).join(''));
          walkDump('after ' + label);
        } catch (e) {
          console.log('FAULT', label, '0x' + va.toString(16));
          walkDump('after ' + label);
        }
      };
      const K = BigInt('0xffff800008000000');
      tryRead('loop-full', BigInt('0xffff800008ec645c'));
      tryRead('loop-lo32', BigInt('0x8ec645c'));
      tryRead('loop-vak', BigInt('0xec645c'));
      tryRead('alt-full', BigInt('0xffff800009798320'));
      tryRead('sock-full', BigInt('0xffff800008e6bf40'));
      tryRead('sock-lo32', BigInt('0x8e6bf40'));
      const pcU = Number(uc.arm64_debug(5)) >>> 0;
      console.log('last-TB ring:', Number(uc.arm64_debug(8)).toString(16),
        Number(uc.arm64_debug(9)), Number(uc.arm64_debug(10)).toString(16),
        '| faultva', (Number(uc.arm64_debug(22)) >>> 0).toString(16),
        '| ttbr_used', (Number(uc.arm64_debug(24)) >>> 0).toString(16),
        '| level', Number(uc.arm64_debug(25)), '| inputsize', Number(uc.arm64_debug(26)));
      try {
        const b = uc.mem_read(pcU, 16);
        console.log('bytes @pc-lo32 0x' + pcU.toString(16) + ':', Array.from(b).map((x) => x.toString(16).padStart(2, '0')).join(' '));
      } catch (e2) {
        console.log('pc-lo32 0x' + pcU.toString(16), 'unmapped');
      }
      // DECISIVE manual 4-level A64 walk of the FULL crash VA via TTBR1.
      try {
        const rdU64 = (reg) => {
          const b = uc.reg_read(reg, 8);
          let v = 0n;
          for (let i = 7; i >= 0; i--) v = (v << 8n) | BigInt(b[i]);
          return v;
        };
        const TTBR = rdU64(ucMod.ARM64_REG_TTBR1_EL1);
        const tcr1 = Number(uc.arm64_debug(60));
        const tcr0 = Number(uc.arm64_debug(61));
        const insize = Number(uc.arm64_debug(62));
        const t1sz = (tcr1 >> 16) & 0x3f;
        const t0sz = tcr1 & 0x3f;
        console.log('  TCR_EL1 raw=0x' + tcr1.toString(16) + ' TCR_EL0 raw=0x' + tcr0.toString(16) +
          ' | T1SZ=' + t1sz + ' T0SZ=' + t0sz + ' | computed inputsize(TTBR1)=' + insize);
        const va = 0xffff8000097342d8n;
        console.log('  TTBR1_EL1 = 0x' + TTBR.toString(16) +
          ' | RAM top = 0x08000000 | VA = 0x' + va.toString(16));
        let base = TTBR & 0x0000FFFFFFFFF000n;
        const shifts = [39, 30, 21, 12];
        for (let lvl = 0; lvl < 4; lvl++) {
          const i = Number((va >> BigInt(shifts[lvl])) & 0x1ffn);
          const descAddr = base + BigInt(i * 8);
          let desc = 0n;
          try {
            const b = uc.mem_read(descAddr, 8);
            for (let j = 7; j >= 0; j--) desc = (desc << 8n) | BigInt(b[j]);
          } catch (e3) {
            console.log('  L' + lvl + ' OOB read @0x' + descAddr.toString(16) +
              ' (base 0x' + base.toString(16) + ' idx ' + i + ')  <-- walk goes out of bounds here');
            break;
          }
          const type = Number(desc & 3n);
          const pa = desc & 0x0000FFFFFFFFF000n;
          console.log('  L' + lvl + ' [' + i + '] @0x' + descAddr.toString(16) +
            ' desc 0x' + desc.toString(16) + ' type ' + type + ' pa 0x' + pa.toString(16));
          if (type === 3) { base = pa; continue; }
          if (type === 1) {
            const off = va & ((1n << BigInt(shifts[lvl])) - 1n);
            console.log('  L' + lvl + ' BLOCK -> pa 0x' + (pa + off).toString(16));
            break;
          }
          console.log('  L' + lvl + ' INVALID descriptor (fault)');
          break;
        }
      } catch (e4) {
        console.log('  walk dump failed:', String(e4).slice(0, 120));
      }
      // Also walk with the VA TRUNCATED to 32 bits (what the fork would do
      // if it truncates the address before the walk).
      try {
        const rdU64b = (reg) => {
          const b = uc.reg_read(reg, 8);
          let v = 0n;
          for (let i = 7; i >= 0; i--) v = (v << 8n) | BigInt(b[i]);
          return v;
        };
        const TTBR = rdU64b(ucMod.ARM64_REG_TTBR1_EL1);
        const vaFull = 0xffff8000097342d8n;
        const va32 = vaFull & 0xffffffffn;
        // QEMU-exact index: shift = 9*(4-level), mask = (1<<12)-1 after a table.
        const walkExact = (label, va) => {
          console.log('  [' + label + '] va 0x' + va.toString(16));
          let base = TTBR & 0x0000FFFFFFFFF000n;
          let oob = false;
          for (let lvl = 0; lvl < 4 && !oob; lvl++) {
            const sh = 9 * (4 - lvl);
            const idx = Number((va >> BigInt(sh)) & 0xfffn);
            const descAddr = base + BigInt(idx * 8);
            let desc = 0n;
            try {
              const b = uc.mem_read(descAddr, 8);
              for (let j = 7; j >= 0; j--) desc = (desc << 8n) | BigInt(b[j]);
            } catch (e5) {
              console.log('    L' + lvl + ' OOB @0x' + descAddr.toString(16) +
                ' idx ' + idx + ' (base 0x' + base.toString(16) + ')');
              oob = true;
              break;
            }
            const type = Number(desc & 3n);
            const pa = desc & 0x0000FFFFFFFFF000n;
            console.log('    L' + lvl + ' [' + idx + '] @0x' + descAddr.toString(16) +
              ' desc 0x' + desc.toString(16) + ' type ' + type + ' pa 0x' + pa.toString(16));
            if (type === 3) { base = pa; continue; }
            if (type === 1) { console.log('    L' + lvl + ' BLOCK ok'); break; }
            console.log('    L' + lvl + ' INVALID'); break;
          }
        };
        walkExact('full-va', vaFull);
        walkExact('trunc-32', va32);
      } catch (e6) {
        console.log('  exact walk failed:', String(e6).slice(0, 120));
      }
      // Dump the KERNEL's real init_idmap_pg_dir (PA 0x19e0000) and walk the
      // kernel _text VA 0xffff800008000000 with BOTH the ARM-correct formula
      // (shift 39/30/21/12, mask 9) and the fork's original (shift 36/27/18/9,
      // mask 12) to see which one finds the kernel's real descriptors.
      try {
        const IDMAP = 0x19e0000n;
        const readU64 = (pa) => {
          const b = uc.mem_read(Number(pa), 8);
          let v = 0n;
          for (let j = 7; j >= 0; j--) v = (v << 8n) | BigInt(b[j]);
          return v;
        };
        const walkIdmap = (label, shifts, mask, vaT) => {
          console.log('  idmap ' + label + ' VA 0x' + vaT.toString(16) + ':');
          let base = IDMAP & 0x0000FFFFFFFFF000n;
          for (let lvl = 0; lvl < 4; lvl++) {
            const idx = Number((vaT >> BigInt(shifts[lvl])) & mask);
            const descAddr = base + BigInt(idx * 8);
            let desc = 0n;
            try { desc = readU64(descAddr); }
            catch (e7) { console.log('    L' + lvl + ' OOB @0x' + descAddr.toString(16)); break; }
            const type = Number(desc & 3n);
            const pa = desc & 0x0000FFFFFFFFF000n;
            console.log('    L' + lvl + ' [' + idx + '] @0x' + descAddr.toString(16) +
              ' desc 0x' + desc.toString(16) + ' type ' + type + ' pa 0x' + pa.toString(16));
            if (type === 3) { base = pa; continue; }
            if (type === 1) { console.log('    L' + lvl + ' BLOCK -> 0x' + (pa + (vaT & ((1n << BigInt(shifts[lvl])) - 1n))).toString(16)); break; }
            console.log('    L' + lvl + ' INVALID'); break;
          }
        };
        console.log('  init_idmap_pg_dir PA=0x' + IDMAP.toString(16) + ' (TTBR0 early, identity map of PHYSICAL VA)');
        walkIdmap('ARM-correct', [39, 30, 21, 12], 0x1ffn, 0x200000n);
        walkIdmap('fork-orig', [36, 27, 18, 9], 0xfffn, 0x200000n);
        // Dump the swapper (TTBR1) for the KIMAGE VA 0xffff800008000000.
        try {
          const SWAP = 0x1909000n;
          const walkSwap = (label, shifts, mask) => {
            const vaT = 0xffff800008000000n;
            console.log('  swapper ' + label + ' VA 0x' + vaT.toString(16) + ' (PA 0x' + SWAP.toString(16) + '):');
            let base = SWAP & 0x0000FFFFFFFFF000n;
            for (let lvl = 0; lvl < 4; lvl++) {
              const idx = Number((vaT >> BigInt(shifts[lvl])) & mask);
              const descAddr = base + BigInt(idx * 8);
              let desc = 0n;
              try { desc = readU64(descAddr); }
              catch (e7) { console.log('    L' + lvl + ' OOB @0x' + descAddr.toString(16)); break; }
              const type = Number(desc & 3n);
              const pa = desc & 0x0000FFFFFFFFF000n;
              console.log('    L' + lvl + ' [' + idx + '] @0x' + descAddr.toString(16) +
                ' desc 0x' + desc.toString(16) + ' type ' + type + ' pa 0x' + pa.toString(16));
              if (type === 3) { base = pa; continue; }
              if (type === 1) { console.log('    L' + lvl + ' BLOCK -> 0x' + (pa + (vaT & ((1n << BigInt(shifts[lvl])) - 1n))).toString(16)); break; }
              console.log('    L' + lvl + ' INVALID'); break;
            }
          };
          console.log('  swapper_pg_dir PA=0x' + SWAP.toString(16));
          walkSwap('ARM-correct', [39, 30, 21, 12], 0x1ffn);
          walkSwap('fork-orig', [36, 27, 18, 9], 0xfffn);
        } catch (e9) {
          console.log('  swapper dump failed:', String(e9).slice(0, 120));
        }
      } catch (e8) {
        console.log('  idmap dump failed:', String(e8).slice(0, 120));
      }
      console.log('--- console so far (' + chars.length + ' chars) ---');
      console.log(chars.slice(-1500));
      console.log('---');
      throw e;
    }
    try { localInt.syncIn(uc); } catch (e) { console.log('SYNCIN localInt THREW:', String(e).slice(0,150)); throw e; }
    try { ic.syncIn(uc); } catch (e) { console.log('SYNCIN ic THREW:', String(e).slice(0,150)); throw e; }
    try { uart0.syncIn(uc); } catch (e) { console.log('SYNCIN uart0 THREW:', String(e).slice(0,150)); throw e; }
    try { localInt.syncIrq(uc, (l) => uc.arm64_set_irq(l)); } catch (e) { console.log('SYNCIRQ localInt THREW:', String(e).slice(0,150)); throw e; }
    steps++;
    return drain();
  };

  const t0 = Date.now();
  let sliced = 0;
   let lastPtw = 0n;
   let spEl0Prev = -1n;
  const walkProbe = (label) => {
    const mk = (s) => Number(uc.arm64_debug(s)).toString(16);
    const pc = Number(uc.arm64_debug(5)).toString(16);
    const r = [];
    for (let i = 0; i < 8; i++) r.push('[' + mk(31 + i) + '/' + mk(39 + i) + ']');
    const rd = Number(uc.arm64_debug(30));
    console.log(`WALK ${label}: pc 0x${pc} ret ${mk(14)} fault ${mk(15)} ttbr0 ${mk(16)} tcr ${mk(17)} faultva ${mk(22)} ttbr_used ${mk(24)} level ${mk(25)} inputsize ${mk(26)} ptw_reads ${rd} (Δ${Number(BigInt(rd) - lastPtw)}) | ring: ${r.join(' ')}`);
    lastPtw = BigInt(rd);
  };
  walkProbe('boot');
  for (; sliced < MAX_SLICES; sliced++) {
    chars += slice();
    if (aborted) { console.log('STOP after first abort'); break; }
    if (sliced % 100 === 0) {
      try {
        const pc = Number(uc.arm64_debug(5)).toString(16);
        const lastTB = Number(uc.arm64_debug(8)).toString(16);
        const rdX = (r) => { const b = uc.reg_read(r, 8); let v = 0n; for (let j = 7; j >= 0; j--) v = (v << 8n) | BigInt(b[j]); return BigInt.asUintN(64, v).toString(16); };
        const x9 = rdX(ucMod.ARM64_REG_X9), x10 = rdX(ucMod.ARM64_REG_X10), x23 = rdX(ucMod.ARM64_REG_X23);
        console.log(`   [hang] slice ${sliced} pc=0x${pc} lastTB=0x${lastTB} x9=0x${x9} x10=0x${x10} x23=0x${x23} chars=${chars.length}`);
      } catch (e) {}
    }
    if (sliced % 50 === 0) {
      try {
        const b = uc.reg_read(ucMod.ARM64_REG_SP_EL0, 8); let v = 0n; for (let j = 7; j >= 0; j--) v = (v << 8n) | BigInt(b[j]);
        if (v !== spEl0Prev) {
          console.log(`   [spel0] slice ${sliced} sp_el0=0x${BigInt.asUintN(64, v).toString(16)} pc=0x${Number(uc.arm64_debug(5)).toString(16)}`);
          spEl0Prev = v;
        }
      } catch (e) {}
    }
    if (sliced % 1000 === 0 && sliced > 0) walkProbe('s' + sliced);
    if (sliced === 4200) walkProbe('pre-crash');
    if (sliced % 5000 === 0 && sliced > 0) {
      const mips = ((sliced * SLICE_INSNS) / ((Date.now() - t0) / 1000) / 1e6).toFixed(2);
      const pc = Number(uc.arm64_debug(5)).toString(16);
      process.stdout.write(`\r${sliced} slices | ${mips} MIPS | ${chars.length} chars | pc 0x${pc}`);
    }
    if (sliced === 10 || sliced === 50 || sliced === 100 || sliced === 500 || sliced === 600 || sliced === 700 || sliced === 800 || sliced === 900 || sliced === 1000 || sliced === 1500 || sliced === 2000 || sliced === 5000 || sliced === 20000 || sliced === 50000 || sliced === 100000 || sliced === 200000 || sliced === 400000 || sliced === 600000) {
      try {
        const csPA = 0x203e650n;
        const buf = uc.mem_read(Number(csPA), 32);
        const csw = [];
        for (let i = 0; i < 32; i += 8) { let v = 0n; for (let j = 7; j >= 0; j--) v = (v << 8n) | BigInt(buf[i + j]); csw.push('0x' + BigInt.asUintN(64, v).toString(16)); }
        console.log('   console_sem@' + csPA.toString(16) + ': ' + csw.join(' '));
        try {
          const SP0 = ucMod.ARM64_REG_SP_EL0;
          if (SP0 !== undefined) {
            const b = uc.reg_read(SP0, 8); let v = 0n; for (let i = 7; i >= 0; i--) v = (v << 8n) | BigInt(b[i]);
            console.log('   SP_EL0 = 0x' + BigInt.asUintN(64, v).toString(16));
          } else { console.log('   ARM64_REG_SP_EL0 undefined in binding'); }
        } catch (e) { console.log('   SP_EL0 read err: ' + e.message); }
        try {
          const itPA = 0x202d100n;
          const ib = uc.mem_read(Number(itPA), 128);
          const itw = [];
          for (let i = 0; i < 128; i += 8) { let v = 0n; for (let j = 7; j >= 0; j--) v = (v << 8n) | BigInt(ib[i + j]); const vs = '0x' + BigInt.asUintN(64, v).toString(16); itw.push(vs); }
          console.log('   init_task@' + itPA.toString(16) + ': ' + itw.join(' '));
        } catch (e) { console.log('   init_task ERR: ' + e.message); }
       } catch (e) { console.log('   console_sem ERR: ' + e.message); }
       if (sliced === 20000) {
         try {
           const NEEDLE = '4e200000730';
           let found = -1, fpa = 0n;
           for (let pa = 0x200000n; pa < 0x8000000n && found < 0; pa += 0x10000n) {
             const b = uc.mem_read(Number(pa), 0x10000);
             const s = String.fromCharCode(...b).replace(/[^\x20-\x7e\n\r\t]/g, '.');
             const i = s.indexOf(NEEDLE);
             if (i >= 0) { found = i; fpa = pa + BigInt(i); }
           }
           if (found >= 0) {
              const b = uc.mem_read(Number(fpa - 0x200n), 0x4000);
             let out = '';
             for (let i = 0; i < b.length; i++) { const c = b[i]; out += (c >= 0x20 && c < 0x7f) ? String.fromCharCode(c) : (c === 0x0a ? '\n' : '.'); }
             console.log('--- RING BUFFER OOPS @0x' + fpa.toString(16) + ' ---\n' + out + '\n--- END OOPS ---');
           } else { console.log('   [oops-scan] "Unable to handle" not found in RAM 0x200000..0x4000000'); }
          } catch (e) { console.log('   oops-scan ERR: ' + e.message); }
     }
     }
     if (sliced === 100000 || sliced === 200000 || sliced === 400000 || sliced === 600000) {
      try {
        const SHIFTS = [39n,30n,21n,12n], ROOT = 0x24ed000n;
        const rd = (id) => { const b = uc.reg_read(id, 8); let v = 0n; for (let i = 7; i >= 0; i--) v = (v << 8n) | BigInt(b[i]); return v; };
        const pcN = Number(uc.arm64_debug(5));
        const lr = rd(ucMod.ARM64_REG_LR);
        const sp = rd(ucMod.ARM64_REG_SP);
        const spPA = walkVA(BigInt.asUintN(64, sp), ROOT, SHIFTS);
        const frames = [];
        for (let i = 0; i < 24; i++) {
          const v = readU64(spPA + BigInt(i * 8));
          frames.push('0x' + BigInt.asUintN(64, v).toString(16));
        }
        console.log(`[unwind] slice ${sliced}: pc=0x${pcN.toString(16)} lr=0x${BigInt.asUintN(64,lr).toString(16)} sp=0x${BigInt.asUintN(64,sp).toString(16)} spPA=0x${spPA?spPA.toString(16):'NULL'}`);
        console.log('   stack: ' + frames.join(' '));
        try {
          const RB = 0xffff800009e3e5e0n;
          const rbPA = walkVA(RB, ROOT, SHIFTS);
          const rbw = [];
          for (let i = 0; i < 256; i += 8) rbw.push('0x' + BigInt.asUintN(64, readU64(rbPA + BigInt(i))).toString(16));
          console.log('   rb@' + rbPA.toString(16) + ': ' + rbw.join(' '));
          try {
            const dataVA = 0xffff80000a26f518n;
            const dPA = walkVA(dataVA, ROOT, SHIFTS);
            const buf = uc.mem_read(Number(dPA), 131072);
            let run = '', runs = [];
            for (let i = 0; i < buf.length; i++) {
              const c = buf[i];
              if (c >= 32 && c < 127) { run += String.fromCharCode(c); }
              else { if (run.length >= 5) runs.push(run); run = ''; }
            }
            if (run.length >= 5) runs.push(run);
            console.log('   RING TEXT (' + runs.length + ' runs):');
            for (const r of runs) console.log('     |' + r.replace(/\n/g, '\\n').slice(0, 200));
          } catch (e) { console.log('   ring scan ERR: ' + e.message); }
        } catch (e) { console.log('   rb ERR: ' + e.message); }
      } catch (e) { console.log('[unwind] slice ' + sliced + ' ERR: ' + e.message); }
    }
    if (chars.includes('Kernel panic')) break;
    if (chars.includes('Unable to mount root fs')) break;
    if (chars.includes('Kernel Offset')) break;
  }
  process.stdout.write('\n');

  const checks = [
    ['earlycon banner:', chars.includes('Booting Linux on physical CPU')],
    ['memory init:', chars.includes('Memory:')],
    ['kernel command line:', chars.includes('Kernel command line')],
    ['interrupt controllers probed:', chars.includes('[    0.000000] IRQ:') || chars.includes('sched_clock')],
    ['console enabled:', chars.includes('console [ttyAMA0] enabled')],
    ['smp brought up (or none):', chars.includes('smp:')],
    ['freeing unused kernel memory:', chars.includes('Freeing unused kernel memory')],
    ['expected end (no rootfs):', chars.includes('VFS: Cannot open root device') || chars.includes('Kernel panic - not syncing: VFS')],
  ];
  for (const [name, ok] of checks) console.log(ok ? 'PASS' : 'FAIL', '-', name);
  console.log('slices:', sliced, '| wall:', Date.now() - t0, 'ms | output:', chars.length, 'chars');
  console.log('--- console (tail) ---');
  console.log(chars.slice(-2000));
  console.log('---');
  uc.close();
  process.exit(checks.every(([, ok]) => ok) ? 0 : 1);
}

main().catch((e) => {
  console.error('FATAL', String(e && e.message || e).slice(0, 300));
  if (e && e.stack) console.error('STACK:\n' + e.stack.split('\n').slice(0, 12).join('\n'));
  process.exit(1);
});