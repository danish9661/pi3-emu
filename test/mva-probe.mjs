import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));

const PROG = join(__dirname, '..', 'public', 'programs', 'mva.elf');
const RAM_BASE = 0x100000;
const RAM_SIZE = 0x300000;
const UART_DR = 0x3f201000;

function readU64(uc, addr) {
  const b = uc.mem_read(addr, 8);
  return BigInt(b[0]) + BigInt(b[1]) * 2n ** 8n + BigInt(b[2]) * 2n ** 16n + BigInt(b[3]) * 2n ** 24n +
         BigInt(b[4]) * 2n ** 32n + BigInt(b[5]) * 2n ** 40n + BigInt(b[6]) * 2n ** 48n + BigInt(b[7]) * 2n ** 56n;
}

function findSymbol(buf, name) {
  const u16 = (o) => buf[o] | (buf[o + 1] << 8);
  const u32 = (o) => buf[o] | (buf[o + 1] << 8) | (buf[o + 2] << 16) | (buf[o + 3] << 24);
  const u64 = (o) => u32(o) + u32(o + 4) * 2 ** 32;
  const shoff = u64(40), shentsize = u16(58), shnum = u16(60);
  for (let i = 0; i < shnum; i++) {
    const o = shoff + i * shentsize;
    if (u32(o + 4) !== 2) continue;
    const link = u32(o + 40), off = u64(o + 24), size = u64(o + 32), ents = u64(o + 56);
    const strOff = u64(shoff + link * shentsize + 24);
    for (let s = 0; s < size / ents; s++) {
      const so = off + s * ents;
      const stName = u32(so), stValue = u64(so + 8);
      let n = strOff + stName, nm = '';
      while (buf[n]) nm += String.fromCharCode(buf[n++]);
      if (nm === name) return stValue;
    }
  }
  throw new Error('symbol not found: ' + name);
}

async function main() {
  const ucMod = await MUnicorn();
  const elfBuf = new Uint8Array(readFileSync(PROG));
  const elf = parseElf(elfBuf);
  const scratch = findSymbol(elfBuf, 'SCRATCH');
  let pass = 0, fail = 0;
  const check = (label, cond, detail = '') => {
    if (cond) { pass++; console.log(`  PASS ${label}${detail ? ' (' + detail + ')' : ''}`); }
    else { fail++; console.log(`  FAIL ${label}${detail ? ' (' + detail + ')' : ''}`); }
  };

  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  uc.mem_map(RAM_BASE, RAM_SIZE, ucMod.PROT_ALL);
  uc.mem_map(0x3f000000, 0x400000, ucMod.PROT_ALL); // PL011 for puts
  loadElf(uc, elf);
  const out = [];
  uc.hook_add(ucMod.HOOK_MEM_WRITE, (u, access, addr, size, value) => {
    if (Number(addr) === UART_DR) out.push(String.fromCharCode(Number(value) & 0xff));
  }, null, UART_DR, UART_DR + 3);

  let fault = null;
  try {
    uc.emu_start(RAM_BASE, 0, 0, 4000000);
  } catch (e) {
    fault = e;
  }
  const text = out.join('');
  console.log('OUT:', JSON.stringify(text));
  check('no exception fault', fault === null,
    fault ? fault.toString().split('\n')[0] : '');
  check('guest reports PASS', text.includes('mva: real MMU: PASS'));
  check('identity read-back intact', readU64(uc, scratch) === 0xDEAD_BEEFn,
    'pa=0x' + readU64(uc, scratch).toString(16));
  check('alias read-back via 0x80000008', readU64(uc, scratch + 8) === 0xCAFE_F00Dn,
    'va=0x' + readU64(uc, scratch + 8).toString(16));
  check('alias read-back via 0x200008', readU64(uc, scratch + 16) === 0xCAFE_F00Dn,
    'pa2=0x' + readU64(uc, scratch + 16).toString(16));
  check('guest completed', readU64(uc, scratch + 24) === 1n);
  check('real LPAE walk happened', Number(uc.arm64_debug(30)) > 0, // ptw read count
    'ptw reads=' + Number(uc.arm64_debug(30)));

  console.log(`\nmva probe: ${pass} passed, ${fail} failed`);
  uc.close();
  process.exit(fail === 0 ? 0 : 1);
}

main().catch((e) => { console.error(e); process.exit(1); });
