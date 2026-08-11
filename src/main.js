import './styles.css';

const UART_WINDOW = 0x1000;
const TX_SLOT_STRIDE = 4;
const TX_SLOTS = 16;
const RAM_BASE = 0x0;
const RAM_SIZE = 0x400000;
const INIT_ADDR = 0x80000;
const ECHO_ADDR = 0x80100;
const ECHO_SLICES = 4;
const SHELL_RUN_SLICES = 256;

// Shell procedure indices exported by the board (pi_shell_*):
//   0 = HI -> "HELLO\r\n"   1 = RPI -> "Raspberry Pi 3\r\n"
//   2 = HELP -> "hi or rpi\r\n"   3 = unknown -> "?\r\n"   4 = prompt -> "> "
const SHELL_CMDS = { HI: 0, RPI: 1, HELP: 2 };

const term = document.getElementById('term');
const status = document.getElementById('status');
const runBtn = document.getElementById('run');

let ucMod = null;
let uc = null;
let board = null;
let rxSlot = 0;
let line = '';

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

// CPU note: this unicorn.js build cannot do conditional branches or flag
// ops at all (b.eq never takes, b.ne always takes, cmp/subs corrupt the
// following instruction, backward branches crash). The guest therefore runs
// only straight-line, host-scheduled procedures built from verified opcodes
// (ldr-literal, movz small imm, str w unsigned-offset, b .):
//   KERNEL_INIT  — prints "Hi\n> ", parks
//   KERNEL_ECHO  — one keystroke: RX slot -> TX slot 0, clears RX
//   SHELL_*      — one per shell response, all printing via the TX window
// The host owns every decision (command dispatch, run counts); the guest
// owns every character of output. Device window (0x3F201000, 4 KiB):
//   +0x00..  TX slots, one char per word (16 slots, host drains)
//   +0x40    RX slot  (host writes a byte, echo procedure consumes)
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
  for (let i = 0; i < 5; i++) {
    loadKernel(board.pi_shell_addr(i), board.pi_shell_proc(i), board.pi_shell_len(i));
  }

  uc.reg_write_i32(ucMod.ARM64_REG_PC, INIT_ADDR);
  uc.reg_write_i32(ucMod.ARM64_REG_SP, RAM_BASE + RAM_SIZE - 16);
}

// Pull TX characters out of the device window into the board console FIFO,
// blank the consumed slots. Returns how many characters were found.
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

function runSlice(addr, count) {
  uc.emu_start(addr, 0, 0, count);
  pumpUart(ucMod, uc, board);
  return drain(board);
}

// One guest keystroke: deliver the byte to the RX slot, let the echo
// procedure move it into TX slot 0.
function guestType(code) {
  uc.mem_write(rxSlot, [code]);
  return runSlice(ECHO_ADDR, ECHO_SLICES);
}

function handleKey(e) {
  if (!uc || runBtn.disabled) return;
  if (e.key === 'Backspace') {
    if (line.length > 0) {
      line = line.slice(0, -1);
      term.textContent = term.textContent.slice(0, -1);
    }
    return;
  }
  const c = e.key.length === 1 ? e.key.charCodeAt(0) : e.key === 'Enter' ? 13 : 0;
  if (!c) return;
  e.preventDefault();

  if (c === 13) {
    const cmd = line.toUpperCase();
    line = '';
    draw(guestType(13)); // CR echo (\r renders invisibly)
    if (cmd.length > 0) {
      const idx = SHELL_CMDS[cmd] ?? 3; // unknown -> "?"
      draw(runSlice(board.pi_shell_addr(idx), SHELL_RUN_SLICES));
      draw(runSlice(board.pi_shell_addr(4), SHELL_RUN_SLICES)); // prompt
    } else {
      draw(runSlice(board.pi_shell_addr(4), SHELL_RUN_SLICES));
    }
    return;
  }

  line += e.key;
  draw(guestType(c));
}

async function run() {
  runBtn.disabled = true;
  term.textContent = '';
  line = '';
  try {
    const MUnicorn = window.MUnicorn;
    if (!MUnicorn) throw new Error('unicorn.js failed to load (check public/unicorn.js)');
    ucMod = await MUnicorn();
    board = await loadBoard();
    uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
    boot(ucMod, uc, board);

    draw(runSlice(INIT_ADDR, 64)); // boot greeting

    setStatus('booted (AArch64 core) — type a command: HI, RPI, HELP');
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