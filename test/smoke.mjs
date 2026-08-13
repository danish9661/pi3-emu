import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));
const { createUart0 } = await import(join(__dirname, '..', 'src', 'uart0.js'));

const UART_WINDOW = 0x1000;
const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const MAX_SLICES = 5000;
const PROG = join(__dirname, '..', 'public', 'programs', 'shell.elf');

async function main() {
  const ucMod = await MUnicorn();
  const board = (
    await WebAssembly.instantiate(
      readFileSync(join(__dirname, '..', 'public', 'pi_board.wasm')),
      {}
    )
  ).instance.exports;

  const uart = Number(board.pi_uart_base());
  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
  uc.mem_map(uart, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  const uart0 = createUart0(uc, ucMod, uart, (b) => board.pi_cons_push(b));

  const elf = parseElf(new Uint8Array(readFileSync(PROG)));
  loadElf(uc, elf);
  // reg_write is a no-op in this unicorn build: the guest _start sets its own
  // SP, and the first slice starts at e_entry via the begin argument.
  let pc = elf.entry;

  let chars = '';
  const drain = () => {
    let found = 0;
    for (;;) {
      const c = Number(board.pi_cons_poll());
      if (c === -1 || c === 0xffffffff) break;
      chars += String.fromCharCode(c);
      found++;
    }
    return found;
  };

  const slice = () => {
    const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || elf.entry;
    uart0.syncOut(uc);
    uc.emu_start(pc, 0, 0, SLICE_INSNS);
    uart0.syncIn(uc);
    return drain();
  };

  // Run slices until the guest goes quiet (2 empty drains) — same as the host.
  const runUntilIdle = () => {
    let quiet = 0;
    for (let i = 0; i < MAX_SLICES && quiet < 2; i++) {
      quiet = slice() === 0 ? quiet + 1 : 0;
    }
  };

  const type = (str) => {
    for (const ch of str) {
      uart0.push(typeof ch === 'number' ? ch : ch.charCodeAt(0));
      runUntilIdle();
    }
  };

  const t0 = Date.now();
  runUntilIdle(); // boot greeting ("Hi\n> ") then park on getc

  // sessions (shell.elf behavior, same expected console as before)
  type('HI\r');
  type('RPI\r');
  type('ZZ\r');
  type(['H', 'X', 0x7f, 'I', '\r']);
  const elapsed = Date.now() - t0;

  const want = 'Hi\n> HI\rHELLO\r\n> RPI\rRaspberry Pi 3\r\n> ZZ\r?\r\n> HXI\rHELLO\r\n> ';
  console.log('console output:', JSON.stringify(chars));
  console.log('console OK:', chars === want);
  if (chars !== want) console.log('want          :', JSON.stringify(want));
  console.log('session time:', elapsed, 'ms');
  uc.close();
}

main().then(() => process.exit(0)).catch((e) => {
  console.error('FATAL', String(e && e.message || e).slice(0, 300));
  process.exit(1);
});