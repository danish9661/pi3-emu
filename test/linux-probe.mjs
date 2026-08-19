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

const SLICE_INSNS = 512;
const LINUX_RAM_SIZE = 0x8000000;
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
function patchDtbRam(dtb) {
  // Walk the FDT structure block looking for the "memory@0" node and its
  // "reg" property. Minimal approach: scan for the 8-byte pattern
  // [0x00,0x00,0x00,0x00, 0x40,0x00,0x00,0x00] followed by nothing we care
  // about — the memory reg is the only <0x0 0x40000000> in the bcm2837 dtb.
  const pat = [0x40, 0x00, 0x00, 0x00];
  for (let i = 0; i + 8 <= dtb.length; i++) {
    if (dtb[i] === 0 && dtb[i + 1] === 0 && dtb[i + 2] === 0 && dtb[i + 3] === 0 &&
        dtb[i + 4] === pat[0] && dtb[i + 5] === pat[1] && dtb[i + 6] === pat[2] && dtb[i + 7] === pat[3]) {
      dtb[i + 4] = 0x08; // 0x08000000
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
  const bootargs = 'earlycon=pl011,0x3f201000 console=ttyAMA0,115200';
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
  uc.mem_map(0, LINUX_RAM_SIZE, ucMod.PROT_ALL);
  // Only the windows modeled below are excluded from the black hole; every
  // other peripheral address stays zero-filled RAM.
  mapBlackHole(uc, ucMod, 0x3f000000, 0x3fa00000, [
    [0x3f00b000, 0x1000], [uart, 0x1000], [0x3f300000, 0x1000],
  ]);
  uc.mem_map(0x3f00b000, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(uart, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(0x3f300000, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(0x40000000, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE);

  writeAll(uc, LINUX_IMAGE, image);
  // The fork's fetch path truncates VAs to 32 bits and walks them through
  // TTBR0 (the idmap). Seed the idmap L2 (init_idmap_pg_dir = phys 0x17e0000
  // with base=0; L1 @0x17e1000, L2 @0x17e2000 — System.map) with alias
  // entries so truncated kernel-image fetches 0x08000000..0x0a300000 resolve
  // to image phys (VA KIMAGE+X -> phys X for the 2M-aligned base) and the
  // KPTI trampoline alias 0x10000000 -> phys 0x1702000 via a 4K L3 table in
  // the (never written by the kernel) reserved_pg_dir page @0x1708000.
  seedIdmapAliases(uc, ucMod);
  writeAll(uc, LINUX_DTB, patchDtbChosen(dtb));
  uc.entry = entry;
  uc.reg_write(ucMod.ARM64_REG_SP, LINUX_RAM_SIZE - 0x10000);
  uc.reg_write(ucMod.ARM64_REG_X0, LINUX_DTB);
  uc.reg_write(ucMod.ARM64_REG_X1, 0);
  uc.reg_write(ucMod.ARM64_REG_X2, 0);
  uc.reg_write(ucMod.ARM64_REG_X3, 0);

  const uart0 = createUart0(uc, ucMod, uart, (b) => board.pi_cons_push(b));
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
  const trace = [];
  let inAlt = false;
  let lastPc = -1n;
  let straight = 0;
  const ALT0 = BigInt('0xffff800009793c84'), ALT1 = BigInt('0xffff8000097e5000');
  uc.hook_add(ucMod.HOOK_CODE, (u, addr, size) => {
    const a64 = BigInt.asUintN(64, BigInt(addr));
    const isAlt = a64 >= ALT0 && a64 < ALT1;
    if (isAlt && !inAlt) {
      trace.push('>>ALT ' + a64.toString(16));
      inAlt = true;
    }
    if (!isAlt) inAlt = false;
    const seq = a64 === lastPc + 4n;
    if (seq) {
      straight++;
    } else {
      if (straight) trace.push('+[' + straight + ']');
      trace.push('0x' + a64.toString(16));
      straight = 0;
    }
    lastPc = a64;
    if (trace.length > 96) trace.shift();
  });
  for (const [kind, type] of [
    ['FETCH', ucMod.HOOK_MEM_FETCH_UNMAPPED],
    ['READ', ucMod.HOOK_MEM_READ_UNMAPPED],
    ['WRITE', ucMod.HOOK_MEM_WRITE_UNMAPPED],
  ]) {
    uc.hook_add(type, (u, access, addr, size, value) => {
      console.log(`UNMAPPED ${kind} @ 0x${Number(addr).toString(16)} pc 0x${(Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || 0).toString(16)} size ${Number(size)}`);
    });
  }
  const drain = () => {
    let out = '';
    for (;;) {
      const ch = Number(board.pi_cons_poll());
      if (ch === -1 || ch === 0xffffffff) break;
      out += String.fromCharCode(ch);
    }
    return out;
  };
  const slice = () => {
    const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || entry;
    localInt.syncOut(uc);
    ic.syncOut(uc);
    uart0.syncOut(uc);
    // arch timer at the real 19.2 MHz rate
    uc.arm64_timer_tick(BigInt(Math.floor((performance.now() - tmrWall0) * 1000 * 19.2)));
    try {
      uc.emu_start(pc, 0, 0, SLICE_INSNS);
    } catch (e) {
      const dbgPc = Number(uc.arm64_debug(5));
      const daif = Number(uc.arm64_debug(1));
      console.log('EXC at slice', steps, ': pc 0x' + dbgPc.toString(16), 'daif', daif.toString(16),
        '| elr', (Number(uc.reg_read_i32(ucMod.ARM64_REG_ELR_EL1)) || 0).toString(16),
        '| spsr', (Number(uc.reg_read_i32(ucMod.ARM64_REG_SPSR_EL1)) || 0).toString(16),
        '| sp 0x' + (Number(uc.reg_read_i32(ucMod.ARM64_REG_SP)) || 0).toString(16));
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
      console.log('--- console so far (' + chars.length + ' chars) ---');
      console.log(chars.slice(-1500));
      console.log('---');
      throw e;
    }
    localInt.syncIn(uc);
    ic.syncIn(uc);
    uart0.syncIn(uc);
    localInt.syncIrq(uc, (l) => uc.arm64_set_irq(l));
    steps++;
    return drain();
  };

  const t0 = Date.now();
  let sliced = 0;
  let lastPtw = 0n;
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
    if (sliced % 1000 === 0 && sliced > 0) walkProbe('s' + sliced);
    if (sliced === 4200) walkProbe('pre-crash');
    if (sliced % 5000 === 0 && sliced > 0) {
      const mips = ((sliced * SLICE_INSNS) / ((Date.now() - t0) / 1000) / 1e6).toFixed(2);
      process.stdout.write(`\r${sliced} slices | ${mips} MIPS | ${chars.length} chars`);
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
  process.exit(1);
});