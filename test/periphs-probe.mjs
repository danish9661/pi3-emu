import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));
const { createUart0 } = await import(join(__dirname, '..', 'src', 'uart0.js'));
const { createRng } = await import(join(__dirname, '..', 'src', 'rng.js'));
const { createTempSensor } = await import(join(__dirname, '..', 'src', 'temp.js'));
const { createClockMgr } = await import(join(__dirname, '..', 'src', 'clockmgr.js'));
const { createI2s } = await import(join(__dirname, '..', 'src', 'i2s.js'));
const { createSpi1 } = await import(join(__dirname, '..', 'src', 'spi1.js'));
const { createSpi2 } = await import(join(__dirname, '..', 'src', 'spi2.js'));
const { createI2c } = await import(join(__dirname, '..', 'src', 'i2c.js'));
const { createUart25 } = await import(join(__dirname, '..', 'src', 'uart25.js'));
const { createUsb } = await import(join(__dirname, '..', 'src', 'usb.js'));

const RAM_SIZE = 0x400000;
const SLICE_INSNS = 4096;
const UART_WINDOW = 0x1000;
const PROG = join(__dirname, '..', 'public', 'programs', 'periphs.elf');

const RNG_BASE   = 0x3F104000;
const CLK_BASE   = 0x3F100000;
const I2S_BASE   = 0x3F203000;
const SPI1_BASE  = 0x3F215000;
const I2C0_BASE  = 0x3F205000;
const SPI2_BASE  = 0x3F215000;
const USB_BASE   = 0x3F980000;
const UART2_BASE = 0x3F216000;
const UART3_BASE = 0x3F217000;
const UART4_BASE = 0x3F218000;
const UART5_BASE = 0x3F219000;

const ucMod = await MUnicorn();
const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
uc.mem_map(0x3f201000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(RNG_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(CLK_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(I2S_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(SPI1_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(I2C0_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(USB_BASE, 0x40000, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(UART2_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(UART3_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(UART4_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
uc.mem_map(UART5_BASE, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);

const elf = parseElf(new Uint8Array(readFileSync(PROG)));
uc.entry = loadElf(uc, elf);

const board = (
  await WebAssembly.instantiate(
    readFileSync(join(__dirname, '..', 'public', 'pi_board.wasm')),
    {}
  )
).instance.exports;
const uart = Number(board.pi_uart_base());

let chars = '';
const uart0 = createUart0(uc, ucMod, uart, (b) => board.pi_cons_push(b));

function drain() {
  let out = '';
  for (;;) {
    const c = Number(board.pi_cons_poll());
    if (c === -1 || c === 0xffffffff) break;
    out += String.fromCharCode(c);
  }
  return out;
}

const rng = createRng(uc, ucMod, RNG_BASE);
const temp = createTempSensor(RNG_BASE);
const clk = createClockMgr(uc, ucMod, CLK_BASE);
const i2s = createI2s(uc, ucMod, I2S_BASE);
const spi1 = createSpi1(uc, ucMod, SPI1_BASE);
const spi2 = createSpi2(uc, ucMod, SPI2_BASE);
const i2c0 = createI2c(uc, ucMod, I2C0_BASE);
const usb = createUsb(uc, ucMod, USB_BASE);
const uart25Bases = [UART2_BASE, UART3_BASE, UART4_BASE, UART5_BASE];
const uart25s = uart25Bases.map((b, i) => createUart25(uc, ucMod, b, i + 2));

let usbDone = false;

function slice() {
  const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || uc.entry;
  rng.syncOut(uc);
  temp.syncOut(uc);
  clk.syncOut(uc);
  i2s.syncOut(uc);
  spi1.syncOut(uc);
  spi2.syncOut(uc);
  i2c0.syncOut(uc);
  usb.syncOut(uc);
  for (const u of uart25s) u.syncOut(uc);
  uart0.syncOut(uc);
  uc.emu_start(pc, 0, 0, SLICE_INSNS);
  rng.syncIn(uc);
  clk.syncIn(uc);
  i2s.syncIn(uc);
  spi1.syncIn(uc);
  spi2.syncIn(uc);
  i2c0.syncIn(uc);
  usb.syncIn(uc);
  for (const u of uart25s) u.syncIn(uc);
  uart0.syncIn(uc);
  chars += drain();
  // Check USB_DONE (0x3F980FF0, moved from 0x54 to avoid GLPMCFG)
  const d = uc.mem_read(USB_BASE + 0xFF0, 4);
  if (d[0] & 1) usbDone = true;
}

for (let i = 0; i < 30000 && !usbDone; i++) slice();
// drain any remaining chars after DONE
for (let i = 0; i < 5; i++) chars += drain();

const want = {
  rngCtrl: chars.includes('periphs: RNG CTRL OK'),
  temperature: chars.includes('periphs: Temperature OK'),
  clockMgr: chars.includes('periphs: Clock Manager OK'),
  i2s: chars.includes('periphs: I2S OK'),
  spi1: chars.includes('periphs: SPI1 ENABLES OK'),
  usb: chars.includes('periphs: USB GSNPSID OK'),
  uart25: chars.includes('periphs: UART2-5 LSR OK'),
  allPass: chars.includes('periphs: ALL PASS'),
  parked: usbDone,
};

console.log('periphs-probe:');
for (const [k, v] of Object.entries(want)) {
  console.log('  ' + (v ? 'ok ' : 'FAIL') + ' ' + k);
}
const pass = Object.values(want).every(Boolean);
console.log(pass ? 'periphs-probe: PASS' : 'periphs-probe: FAIL');
if (!pass) {
  console.log('--- guest output ---');
  console.log(chars.replace(/\r/g, ''));
}
uc.close();
process.exit(pass ? 0 : 1);
