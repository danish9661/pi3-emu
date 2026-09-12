import MUnicornPkg from '@alexaltea/unicorn-js';
const MUnicorn = MUnicornPkg.default ?? MUnicornPkg;
const ucMod = await MUnicorn();

const KERNS = {
  b: [0x00, 0x00, 0x00, 0x14], // b .
  mov_b: [0x20, 0x00, 0x80, 0x52, 0x00, 0x00, 0x00, 0x14],
  add_b: [0x00, 0x04, 0x00, 0x11, 0x00, 0x00, 0x00, 0x14],
  nop_b: [0x1f, 0x20, 0x03, 0xd5, 0x00, 0x00, 0x00, 0x14],
};

for (const [name, code] of Object.entries(KERNS)) {
  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_ARM);
  uc.mem_map(0, 0x400000, ucMod.PROT_ALL);
  for (let i = 0; i < code.length; i++) uc.mem_write(0x80000 + i * 4, [code[i]]);
  uc.reg_write_i64(ucMod.ARM64_REG_PC, 0x80000n);
  const seen = [];
  uc.hook_add(ucMod.HOOK_CODE, (h, addr, size) => { seen.push(Number(addr).toString(16)); }, {}, 1, 0);
  try {
    uc.emu_start(0x80000, 0x80000 + (code.length % 4 === 0 ? code.length : code.length + 1), 0, 0);
    console.log(name, 'OK', 'seen:', seen.join(','));
  } catch (e) {
    console.log(name, 'EXC', 'seen:', seen.join(',') || '(none)', '|', String(e).slice(0, 90));
  }
  uc.close();
}