// 2-source differential: unicorn oracle vs pi-cpu `cargo run --example shift`.
// PASS iff every shape agrees.
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
const require = createRequire('/home/danish1075/Documents/ri pi emu/packages/pi3-emu/src/index.js');
const MUnicorn = require('/home/danish1075/Documents/ri pi emu/public/unicorn.js');
const ucMod = await MUnicorn();
const ROOT = '/home/danish1075/Documents/ri pi emu';
const w8 = (x) => { const o = []; for (let i = 0; i < 8; i++) { o.push(Number(x & 0xffn)); x >>= 8n; } return o; };
const w4 = (x) => [x & 0xff, (x >>> 8) & 0xff, (x >>> 16) & 0xff, (x >>> 24) & 0xff];
const cases = [
  [0b1000, 1, 1n, 21n],
  [0b1000, 0, 0x12345678n, 9n],
  [0b1001, 1, 0x123456789abcdef0n, 13n],
  [0b1001, 0, 0x12345678n, 4n],
  [0b1010, 1, 0x8000000000000001n, 1n],
  [0b1010, 0, 0x80000001n, 1n],
  [0b1011, 1, 0x123456789abcdef0n, 8n],
  [0b1011, 0, 0x12345678n, 8n],
  [0b0010, 1, 100n, 7n],
  [0b0010, 0, 100n, 7n],
  [0b0010, 1, 100n, 0n],
  [0b0011, 1, 100n, 7n],
  [0b0011, 0, 100n, 7n],
  [0b0011, 1, 100n, 0n],
  [0b0011, 1, 0x8000000000000000n, 0xffffffffffffffffn],
];
async function oracle(op2, sf, a, b) {
  const wd = (((sf ? 0x9ac00000 : 0x1ac00000) | (5 << 16) | (op2 << 10) | (4 << 5) | 3) >>> 0);
  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  uc.mem_map(0, 0x1000, ucMod.PROT_ALL);
  uc.mem_write(0x100, w4(wd));
  uc.reg_write_i64(ucMod.ARM64_REG_X4, a);
  uc.reg_write_i64(ucMod.ARM64_REG_X5, b);
  uc.emu_start(0x100, 0x104, 0, 1);
  const v = BigInt(uc.reg_read_i64(ucMod.ARM64_REG_X3));
  return sf ? (v & 0xffffffffffffffffn) : (v & 0xffffffffn);
}
const out = execFileSync('cargo', ['run', '--quiet', '--example', 'shift'], { cwd: ROOT + '/cpu' }).toString().trim().split('\n');
let pass = 0;
for (let i = 0; i < cases.length; i++) {
  const [op2, sf, a, b] = cases[i];
  const want = await oracle(op2, sf, a, b);
  const parts = out[i].trim().split(/\s+/);
  const ok = parts[2] !== 'FAULT' && BigInt('0x' + parts[2]) === want;
  if (ok) pass++;
  console.log(`op2=${op2.toString(2).padStart(6, '0')} sf=${sf}: want=${want.toString(16).padStart(16, '0')} got=${parts[2]} ${ok ? 'OK' : 'MISMATCH'}`);
}
console.log(`${pass}/${cases.length} ${pass === cases.length ? 'PASS' : 'FAIL'}`);
process.exit(pass === cases.length ? 0 : 1);
