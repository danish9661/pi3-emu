import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));

const UART_WINDOW = 0x1000;
const TX_SLOT_STRIDE = 4;
const TX_SLOTS = 8;
const RAM_SIZE = 0x400000;
const KERNEL_ADDR = 0x80000;
const INSTRUCTIONS_PER_CHUNK = 16;

async function main() {
  const ucMod = await MUnicorn();
  const board = (
    await WebAssembly.instantiate(
      readFileSync(join(__dirname, '..', 'public', 'pi_board.wasm')),
      {}
    )
  ).instance.exports;

  const uart = Number(board.pi_uart_base());
  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM, ucMod.MODE_ARM);
  uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
  uc.mem_map(uart, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);

  const kptr = Number(board.pi_kernel());
  const klen = Number(board.pi_kernel_len());
  const kernel = new Uint8Array(board.memory.buffer, kptr, klen);
  for (let i = 0; i < klen; i++) uc.mem_write(KERNEL_ADDR + i, [kernel[i]]);

  uc.reg_write_i32(ucMod.ARM_REG_PC, KERNEL_ADDR);
  uc.reg_write_i32(ucMod.ARM_REG_SP, RAM_SIZE - 16);

  const t0 = Date.now();
  let total = 0;
  let quiet = 0;
  let chars = '';
  let pc = KERNEL_ADDR;

  while (quiet < 4) {
    uc.emu_start(pc, 0, 0, INSTRUCTIONS_PER_CHUNK);
    pc = Number(uc.reg_read_i32(ucMod.ARM_REG_PC));
    total += INSTRUCTIONS_PER_CHUNK;

    // drain TX slots: each slot is a word-aligned byte; consume + clear.
    const window = uc.mem_read(uart, TX_SLOTS * TX_SLOT_STRIDE);
    let found = 0;
    for (let i = 0; i < TX_SLOTS; i++) {
      const c = window[i * TX_SLOT_STRIDE];
      if (c !== 0) {
        found++;
        board.pi_cons_push(c);
        for (let k = 0; k < TX_SLOT_STRIDE; k++) {
          uc.mem_write(uart + i * TX_SLOT_STRIDE + k, [0]);
        }
      }
    }
    for (;;) {
      const c = Number(board.pi_cons_poll());
      if (c === -1 || c === 0xffffffff) break;
      chars += String.fromCharCode(c);
    }
    quiet = found > 0 ? 0 : quiet + 1;
  }
  const elapsed = Date.now() - t0;

  const got = JSON.stringify(chars);
  console.log('console output:', got);
  console.log('console OK:', got === '"Hi\\n"');
  console.log(
    'instructions:', total,
    'in', elapsed, 'ms', '≈', Math.round((total / elapsed) * 1000), 'ips'
  );
  uc.close();
}

main().then(() => process.exit(0)).catch((e) => {
  console.error('FATAL', String(e).slice(0, 300));
  process.exit(1);
});