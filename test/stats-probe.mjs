import { createRequire } from 'module';
import { readFileSync } from 'fs';
const require = createRequire(import.meta.url);

const MUnicorn = (await import(/* @vite-ignore */ '../public/unicorn.js')).default
  ?? (await import(/* @vite-ignore */ '../public/unicorn.js'));

const ucMod = await MUnicorn();
const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);

const KERNEL_ADDR = 0x80000;
uc.mem_map(0, 0x400000, ucMod.PROT_ALL);
const bytes = readFileSync('public/pi_board.wasm');
const { instance } = await WebAssembly.instantiate(bytes, {});
const board = instance.exports;
const uart = Number(board.pi_uart_base());
uc.mem_map(uart, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE);
const kern = new Uint8Array(board.memory.buffer, Number(board.pi_kernel()), Number(board.pi_kernel_len()));
for (let i = 0; i < kern.length; i++) uc.mem_write(KERNEL_ADDR + i, [kern[i]]);
uc.reg_write_i32(ucMod.ARM64_REG_PC, KERNEL_ADDR);
uc.reg_write_i32(ucMod.ARM64_REG_SP, 0x400000 - 16);

const t0 = performance.now();
uc.emu_start(KERNEL_ADDR, 0, 0, 512);
const emuMs = performance.now() - t0;

const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC));
const sp = Number(uc.reg_read_i32(ucMod.ARM64_REG_SP));
const mips = (512 / emuMs / 1000).toFixed(2);

console.log(`pc 0x80000+0x${(pc - KERNEL_ADDR).toString(16).padStart(6, '0')}`);
console.log(`sp 0x${sp.toString(16)}`);
console.log(`insns 512 | emu ${emuMs.toFixed(2)}ms | mips ${mips}`);
