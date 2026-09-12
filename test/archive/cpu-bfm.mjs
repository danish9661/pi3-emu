// BFM differential: unicorn oracle vs pi-cpu `cargo run --example bfm`.
// PASS iff every shape agrees (faults count as mismatch unless oracle faults).
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
const require = createRequire('/home/danish1075/Documents/ri pi emu/packages/pi3-emu/src/index.js');
const MUnicorn = require('/home/danish1075/Documents/ri pi emu/public/unicorn.js');
const ucMod = await MUnicorn();
const ROOT = '/home/danish1075/Documents/ri pi emu';
const w8 = (x) => { const o = []; for (let i = 0; i < 8; i++) { o.push(Number(x & 0xffn)); x >>= 8n; } return o; };
const w4 = (x) => [x & 0xff, (x >>> 8) & 0xff, (x >>> 16) & 0xff, (x >>> 24) & 0xff];
const cases = [
  [28, 7, 1, 0x123456789abcdef0n, 0x0fedcba987654321n],
  [13, 5, 1, 0x123456789abcdef0n, 0x0fedcba987654321n],
  [52, 32, 1, 0x123456789abcdef0n, 0x0fedcba987654321n],
  [60, 7, 1, 0xdeadbeefcafef00dn, 0x0123456789abcdefn],
  [4, 11, 1, 0x123456789abcdef0n, 0x0fedcba987654321n],
  [8, 40, 1, 0x123456789abcdef0n, 0x0fedcba987654321n],
  [2, 60, 1, 0x123456789abcdef0n, 0x0fedcba987654321n],
  [10, 10, 1, 0x123456789abcdef0n, 0x0fedcba987654321n],
  [0, 63, 1, 0x123456789abcdef0n, 0x0fedcba987654321n],
  [10, 4, 0, 0x12345678n, 0x0fedcba9n],
  [4, 11, 0, 0x12345678n, 0x0fedcba9n],
  [0, 31, 0, 0x12345678n, 0x0fedcba9n],
  [20, 15, 0, 0xdeadbeefn, 0x01234567n],
  [7, 23, 0, 0xdeadbeefn, 0x01234567n],
];
async function oracle(R, S, sf, src, dst) {
  const wd = ((sf ? 0xb3400000 : 0x33000000) | (R << 16) | (S << 10) | 0x23) >>> 0;
  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  uc.mem_map(0, 0x1000, ucMod.PROT_ALL);
  [0x58000061, 0x58000083, wd].forEach((v, i) => uc.mem_write(0x100 + 4 * i, w4(v)));
  uc.mem_write(0x10c, w8(src));
  uc.mem_write(0x114, w8(dst));
  uc.emu_start(0x100, 0x10c, 0, 8);
  const v = BigInt(uc.reg_read_i64(ucMod.ARM64_REG_X3));
  return sf ? (v & 0xffffffffffffffffn) : (v & 0xffffffffn);
}
const out = execFileSync('cargo', ['run', '--quiet', '--example', 'bfm'], { cwd: ROOT + '/cpu' }).toString().trim().split('\n');
let pass = 0;
for (let i = 0; i < cases.length; i++) {
  const [R, S, sf, src, dst] = cases[i];
  const want = await oracle(R, S, sf, src, dst);
  const got = BigInt('0x' + out[i].trim().split(/\s+/)[3]);
  const ok = got === want;
  if (ok) pass++;
  console.log(`R=${R} S=${S} sf=${sf}: want=${want.toString(16).padStart(16, '0')} got=${got.toString(16).padStart(16, '0')} ${ok ? 'OK' : 'MISMATCH'}`);
}
console.log(`${pass}/${cases.length} ${pass === cases.length ? 'PASS' : 'FAIL'}`);
process.exit(pass === cases.length ? 0 : 1);
