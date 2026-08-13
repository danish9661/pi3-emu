import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';
const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));

const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));

const ucMod = await MUnicorn();
const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);

const RAM_SIZE = 0x400000;
const KERNEL_ADDR = 0x100000;
uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
const bytes = readFileSync(join(__dirname, '..', 'public', 'pi_board.wasm'));
const { instance } = await WebAssembly.instantiate(bytes, {});
const board = instance.exports;
const uart = Number(board.pi_uart_base());
uc.mem_map(uart, 0x1000, ucMod.PROT_READ | ucMod.PROT_WRITE);
const elf = parseElf(new Uint8Array(readFileSync(join(__dirname, '..', 'public', 'programs', 'shell.elf'))));
loadElf(uc, elf);
// reg_write is a no-op in this unicorn build; guest _start sets its own SP

const t0 = performance.now();
uc.emu_start(elf.entry, 0, 0, 512);
const emuMs = performance.now() - t0;

const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC));
const sp = Number(uc.reg_read_i32(ucMod.ARM64_REG_SP));
const mips = (512 / emuMs / 1000).toFixed(2);

console.log(`pc 0x100000+0x${(pc - KERNEL_ADDR).toString(16).padStart(6, '0')}`);
console.log(`sp 0x${sp.toString(16)}`);
console.log(`insns 512 | emu ${emuMs.toFixed(2)}ms | mips ${mips}`);
