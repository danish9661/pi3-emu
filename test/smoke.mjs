import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));

const UART_WINDOW = 0x1000;
const TX_SLOT_STRIDE = 4;
const TX_SLOTS = 32;
const RAM_SIZE = 0x400000;
const KERNEL_ADDR = 0x80000;
const KERNEL_SLICES = 512;

async function main() {
  const ucMod = await MUnicorn();
  const board = (
    await WebAssembly.instantiate(
      readFileSync(join(__dirname, '..', 'public', 'pi_board.wasm')),
      {}
    )
  ).instance.exports;

  const uart = Number(board.pi_uart_base());
  const rx = uart + Number(board.pi_rx_offset());
  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
  uc.mem_map(uart, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);

  const loadKernel = (addr, ptr, len) => {
    const bytes = new Uint8Array(board.memory.buffer, Number(ptr), Number(len));
    for (let i = 0; i < bytes.length; i++) uc.mem_write(addr + i, [bytes[i]]);
  };
  loadKernel(KERNEL_ADDR, board.pi_kernel(), board.pi_kernel_len());

  uc.reg_write_i32(ucMod.ARM64_REG_PC, KERNEL_ADDR);
  uc.reg_write_i32(ucMod.ARM64_REG_SP, RAM_SIZE - 16);

  let chars = '';
  const drain = () => {
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
    return found;
  };

  const slice = () => {
    const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC));
    uc.emu_start(pc, 0, 0, KERNEL_SLICES);
    drain();
  };

  const type = (str) => {
    for (const ch of str) {
      uc.mem_write(rx, [typeof ch === 'number' ? ch : ch.charCodeAt(0)]);
      slice();
    }
  };

  const t0 = Date.now();
  // 1. boot greeting (starts at KERNEL_ADDR; afterwards slices resume from PC)
  uc.reg_write_i32(ucMod.ARM64_REG_PC, KERNEL_ADDR);
  slice();

  // 2. session 1: HI
  type('HI\r');
  // 3. session 2: RPI
  type('RPI\r');
  // 4. session 3: unknown ZZ
  type('ZZ\r');
  // 5. backspace editing: H X <bs> I \r -> buffer "HI" -> HELLO
  type(['H', 'X', 0x7f, 'I', '\r']);
  const elapsed = Date.now() - t0;

  const want = 'Hi\n> HI\rHELLO\r\n> RPI\rRaspberry Pi 3\r\n> ZZ\r?\r\n> HXI\rHELLO\r\n> ';
  const got = JSON.stringify(chars);
  console.log('console output:', got);
  console.log('console OK:', chars === want);
  if (chars !== want) console.log('want          :', JSON.stringify(want));
  console.log('session time:', elapsed, 'ms');
  uc.close();
}

main().then(() => process.exit(0)).catch((e) => {
  console.error('FATAL', String(e && e.message || e).slice(0, 300));
  process.exit(1);
});
