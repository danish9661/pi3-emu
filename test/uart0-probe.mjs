import { createRequire } from 'module';
import { readFileSync } from 'fs';
import { dirname, join } from 'path';
import { fileURLToPath } from 'url';

const require = createRequire(import.meta.url);
const __dirname = dirname(fileURLToPath(import.meta.url));
const MUnicorn = require(join(__dirname, '..', 'public', 'unicorn.js'));
const { parseElf, loadElf } = await import(join(__dirname, '..', 'src', 'elf.js'));
const { createUart0 } = await import(join(__dirname, '..', 'src', 'uart0.js'));
const { createIc } = await import(join(__dirname, '..', 'src', 'ic.js'));

const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const UART_WINDOW = 0x1000;
const PROG = join(__dirname, '..', 'public', 'programs', 'uart0.elf');

const IC_BASE = 0x3f00b200;
const IC_IRQ_RET = IC_BASE + 0x2c;

function writeU32(uc, addr, v) {
  uc.mem_write(addr, [v & 0xff, (v >>> 8) & 0xff, (v >>> 16) & 0xff, (v >>> 24) & 0xff]);
}
function readU32(uc, addr) {
  const b = uc.mem_read(addr, 4);
  return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24);
}

async function main() {
  const ucMod = await MUnicorn();
  const board = (
    await WebAssembly.instantiate(
      readFileSync(join(__dirname, '..', 'public', 'pi_board.wasm')),
      {}
    )
  ).instance.exports;
  const uart = Number(board.pi_uart_base());
  const elf = parseElf(new Uint8Array(readFileSync(PROG)));

  const uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
  uc.mem_map(0, RAM_SIZE, ucMod.PROT_ALL);
  uc.mem_map(uart, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.mem_map(0x3f00b000, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.entry = loadElf(uc, elf);

  const uart0 = createUart0(uc, ucMod, uart, (b) => board.pi_cons_push(b));
  const { state, syncOut, syncIn, push, irqActive } = uart0;

  let chars = '';
  let delivered = 0;
  let irqElr = 0;
  let irqInFlight = false;
  let irqVector = 0;
  let irqResume = 0;

  // The real 3-bank legacy IC (src/ic.js), fed by the PL011's line.
  const ic = createIc(uc, ucMod, IC_BASE, () => ({
    timer: 0,
    dma0: false,
    pl011: irqActive(),
    sdhci: false,
    gpio0: false,
    gpio1: false,
    aux: false,
  }));
  const syncIrqRet = () => {
    const r = readU32(uc, IC_IRQ_RET);
    if (r !== 0) {
      writeU32(uc, IC_IRQ_RET, 0);
      irqResume = irqElr;
    }
  };
  const irqDeliver = () => {
    if (irqInFlight) return;
    if (((Number(uc.arm64_debug(1)) >> 7) & 1) === 1) return; // DAIF.I set
    const p = ic.pending();
    if ((p.b1 | p.b2 | p.basic) === 0) return;
    irqElr = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || elf.entry;
    irqInFlight = true;
    const vbar = Number(uc.reg_read_i32(ucMod.ARM64_REG_VBAR_EL1)) || 0x100000;
    irqVector = vbar + 0x280;
    delivered++;
  };
  const drain = () => {
    let out = '';
    for (;;) {
      const ch = Number(board.pi_cons_poll());
      if (ch === -1 || ch === 0xffffffff) break;
      out += String.fromCharCode(ch);
    }
    return out;
  };
  const slice = () => {
    const pc = irqResume || irqVector || Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || elf.entry;
    if (irqResume) irqInFlight = false;
    irqResume = 0;
    irqVector = 0;
    ic.syncOut(uc);
    syncOut(uc);
    uc.emu_start(pc, 0, 0, SLICE_INSNS);
    ic.syncIn(uc);
    syncIrqRet();
    syncIn(uc);
    const out = drain();
    irqDeliver();
    return out;
  };

  // Phase 1: boot + configure + arm RXINTR (IRQ 57).
  const t0 = Date.now();
  for (let i = 0; i < 30000 && !chars.includes('type a key'); i++) chars += slice();

  // Phase 2: send a key -> RXINTR -> the handler pops DR and reports MIS.
  push(0x48); // 'H' into the PL011 RX FIFO
  for (let i = 0; i < 30000 && !chars.includes("[rx 'H']"); i++) chars += slice();

  // Phase 3: the guest arms TXIM -> the line asserts immediately (TXFE
  // always set) -> the handler de-arms it -> no storm after the de-arm.
  for (let i = 0; i < 30000 && !chars.includes('TX phase done'); i++) chars += slice();

  const txFired = (chars.match(/TXINTR fired/g) || []).length;
  const checks = [
    ['PL011 banner:', chars.includes('uart0: BCM2837 PL011 @ 0x3F201000')],
    ['config latched (IBRD 1, FBRD 40, LCRH 0x70, CR 0x301):', chars.includes('IBRD 1 FBRD 28 (115200 @ 3 MHz), LCRH 0x70, CR 0x301')],
    ['FR TX-ready verified:', chars.includes('FR shows TXFE set, TXFF clear')],
    ['RXINTR armed:', chars.includes('RXINTR armed (IMSC bit 4) -> IRQ 57')],
    ['RXINTR fired with MIS 0x10:', chars.includes('RXINTR fired (MIS 0x10)')],
    ['key echoed via DR read:', chars.includes("[rx 'H']")],
    ['IRQ delivered through the IC:', delivered >= 1],
    ['TXIM armed:', chars.includes('TXIM armed (IMSC bit 5)')],
    ['TXINTR fired (MIS 0x20):', chars.includes('TXINTR fired (MIS 0x20)')],
    ['TXIM de-armed by the handler:', chars.includes('TXIM de-armed')],
    ['TX phase completed:', chars.includes('TX phase done - no IRQ storm after the de-arm')],
    ['TXINTR fired exactly once (no storm):', txFired === 1],
  ];
  for (const [name, ok] of checks) console.log(ok ? 'PASS' : 'FAIL', '-', name);
  console.log('deliveries:', delivered, '| config:', JSON.stringify(state));
  console.log('console output:', JSON.stringify(chars));
  console.log('session time:', Date.now() - t0, 'ms');
  uc.close();
  process.exit(checks.every(([, ok]) => ok) ? 0 : 1);
}

main().catch((e) => {
  console.error('FATAL', String(e && e.message || e).slice(0, 300));
  process.exit(1);
});
