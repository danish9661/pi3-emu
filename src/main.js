import './styles.css';

const UART_WINDOW = 0x1000;
const TX_SLOT_STRIDE = 4;
const TX_SLOTS = 8;
const RAM_BASE = 0x0;
const RAM_SIZE = 0x400000;
const INIT_ADDR = 0x80000;
const ECHO_ADDR = 0x80100;
const ECHO_SLICES = 4;

const term = document.getElementById('term');
const status = document.getElementById('status');
const runBtn = document.getElementById('run');

let ucMod = null;
let uc = null;
let board = null;
let rxSlot = 0;

function setStatus(text) {
  status.textContent = text;
}

function draw(text) {
  term.textContent += text;
  term.scrollTop = term.scrollHeight;
}

async function loadBoard() {
  const resp = await fetch('/pi_board.wasm');
  const { instance } = await WebAssembly.instantiate(await resp.arrayBuffer(), {});
  return instance.exports;
}

// DEVICE NOTES (see board/src/lib.rs for the full archaeology):
// This unicorn.js build has a broken ARM32 decoder (every guest load traps)
// and unreliable conditional branches + wide immediate encodings on AArch64.
// The board therefore ships two *unconditional* AArch64 procedures that only
// use verified opcodes, and the host schedules short slices of them:
//   KERNEL_INIT — prints "Hi\n> " through the TX window, parks on `b .`
//   KERNEL_ECHO — one keystroke: RX slot -> TX slot 0, then clears RX
// Device window (UART @ 0x3F201000, 4 KiB):
//   +0x00..  TX slots, one char per word (guest stores, host drains)
//   +0x40    RX slot  (host writes a byte, echo procedure consumes it)
function boot(ucMod, uc, board) {
  const uart = Number(board.pi_uart_base());
  rxSlot = uart + Number(board.pi_rx_offset());

  uc.mem_map(RAM_BASE, RAM_SIZE, ucMod.PROT_ALL);
  uc.mem_map(uart, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.devUart = { base: uart };

  const loadKernel = (addr, ptr, len) => {
    const bytes = new Uint8Array(board.memory.buffer, Number(ptr), Number(len));
    for (let i = 0; i < bytes.length; i++) uc.mem_write(addr + i, [bytes[i]]);
  };
  loadKernel(INIT_ADDR, board.pi_kernel_init(), board.pi_kernel_init_len());
  loadKernel(ECHO_ADDR, board.pi_kernel_echo(), board.pi_kernel_echo_len());

  uc.reg_write_i32(ucMod.ARM64_REG_PC, INIT_ADDR);
  uc.reg_write_i32(ucMod.ARM64_REG_SP, RAM_BASE + RAM_SIZE - 16);
}

// Pull TX characters out of the device window into the board console FIFO
// and blank the consumed slots. Returns how many characters were found.
function pumpUart(ucMod, uc, board) {
  const { base } = uc.devUart;
  const window = uc.mem_read(base, TX_SLOTS * TX_SLOT_STRIDE);
  let found = 0;
  for (let i = 0; i < TX_SLOTS; i++) {
    const c = window[i * TX_SLOT_STRIDE];
    if (c !== 0) {
      found++;
      board.pi_cons_push(c);
      for (let k = 0; k < TX_SLOT_STRIDE; k++) {
        uc.mem_write(base + i * TX_SLOT_STRIDE + k, [0]);
      }
    }
  }
  return found;
}

function drain(board) {
  let out = '';
  for (;;) {
    const c = Number(board.pi_cons_poll());
    if (c === -1 || c === 0xffffffff) break;
    out += String.fromCharCode(c);
  }
  return out;
}

function handleKey(e) {
  if (!uc || runBtn.disabled) return;
  const c = e.key.length === 1 ? e.key.charCodeAt(0) : e.key === 'Enter' ? 13 : 0;
  if (!c) return;
  e.preventDefault();
  uc.mem_write(rxSlot, [c]);
  uc.emu_start(ECHO_ADDR, 0, 0, ECHO_SLICES);
  pumpUart(ucMod, uc, board);
  draw(drain(board));
}

async function run() {
  runBtn.disabled = true;
  term.textContent = '';
  try {
    const MUnicorn = window.MUnicorn;
    if (!MUnicorn) throw new Error('unicorn.js failed to load (check public/unicorn.js)');
    ucMod = await MUnicorn();
    board = await loadBoard();
    uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
    boot(ucMod, uc, board);

    // boot greeting: run the init procedure to completion (it parks on `b .`)
    uc.emu_start(INIT_ADDR, 0, 0, 64);
    pumpUart(ucMod, uc, board);
    draw(drain(board));

    setStatus('booted (AArch64 core) — type to interact');
    runBtn.textContent = 'Reboot';
    runBtn.disabled = false;
  } catch (err) {
    setStatus('ERROR: ' + err.message);
    console.error(err);
    runBtn.disabled = false;
  }
}

window.addEventListener('keydown', handleKey);
runBtn.addEventListener('click', run);
run();