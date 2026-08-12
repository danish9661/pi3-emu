import './styles.css';

const UART_WINDOW = 0x1000;
const TX_SLOT_STRIDE = 4;
const TX_SLOTS = 32;
const RAM_BASE = 0x0;
const RAM_SIZE = 0x400000;
const KERNEL_ADDR = 0x80000;
const KERNEL_SLICES = 512;

const term = document.getElementById('term');
const status = document.getElementById('status');
const runBtn = document.getElementById('run');
const statsEl = document.getElementById('stats');
const hint = document.getElementById('hint');

let ucMod = null;
let uc = null;
let board = null;
let rxSlot = 0;

let stats = { steps: 0, insns: 0, emuMs: 0, chars: 0, wallStart: 0 };

function setStatus(text) {
  status.textContent = text;
}

function draw(text) {
  term.textContent += text;
  term.scrollTop = term.scrollHeight;
}

async function loadBoard() {
  const resp = await fetch('./pi_board.wasm');
  const { instance } = await WebAssembly.instantiate(await resp.arrayBuffer(), {});
  return instance.exports;
}

// M4: the guest kernel owns everything after boot — RX polling, key echo,
// line buffer, command dispatch, responses, prompt.  The host only delivers
// keystrokes to the RX slot, runs a bounded slice of the kernel, and drains
// TX slots to the console FIFO.  Device window (0x3F201000, 4 KiB):
//   +0x00..  TX slots, one char per word (16 slots, host drains)
//   +0x40    RX slot  (host writes a byte, guest consumes)
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
  loadKernel(KERNEL_ADDR, board.pi_kernel(), board.pi_kernel_len());

  uc.reg_write_i32(ucMod.ARM64_REG_PC, KERNEL_ADDR);
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
      stats.chars++;
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
  const t0 = performance.now();
  uc.emu_start(addr, 0, 0, count);
  stats.emuMs += performance.now() - t0;
  stats.steps += 1;
  stats.insns += count;
  pumpUart(ucMod, uc, board);
  return drain(board);
}

function updateStats() {
  const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC));
  const sp = Number(uc.reg_read_i32(ucMod.ARM64_REG_SP));
  const wall = (performance.now() - stats.wallStart) / 1000;
  const mips = stats.emuMs > 0 ? (stats.insns / stats.emuMs / 1000).toFixed(2) : '—';
  const pcs = (pc - KERNEL_ADDR).toString(16).padStart(6, '0');
  statsEl.innerHTML =
    `<span><span class="k">pc</span> 0x80000+0x${pcs}</span>` +
    `<span><span class="k">sp</span> 0x${sp.toString(16)}</span>` +
    `<span><span class="k">mips</span> ${mips}</span>` +
    `<span><span class="k">steps</span> ${stats.steps}</span>` +
    `<span><span class="k">insns</span> ${stats.insns}</span>` +
    `<span><span class="k">emu</span> ${stats.emuMs.toFixed(2)}ms</span>` +
    `<span><span class="k">wall</span> ${wall.toFixed(2)}s</span>` +
    `<span><span class="k">chars</span> ${stats.chars}</span>`;
}

// The guest kernel is a live process: it parks in its RX poll loop between
// host slices, so every keystroke slice resumes from the current PC.
function guestSlice() {
  const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC));
  const out = runSlice(pc, KERNEL_SLICES);
  updateStats();
  return out;
}

// Deliver one keystroke to the guest kernel, let it do all the work.
function guestKey(code) {
  uc.mem_write(rxSlot, [code]);
  return guestSlice();
}

function handleKey(e) {
  if (!uc || runBtn.disabled) return;
  if (e.key === 'Backspace') {
    e.preventDefault();
    if (term.textContent.length > 0) {
      term.textContent = term.textContent.slice(0, -1);
    }
    draw(guestKey(0x7f)); // guest unwrites its line buffer
    return;
  }
  const c = e.key.length === 1 ? e.key.charCodeAt(0) : e.key === 'Enter' ? 13 : 0;
  if (!c) return;
  e.preventDefault(); // also stops the browser re-clicking a focused button on Enter
  draw(guestKey(c));
}

// On-screen keyboard: feed the same guestKey path as physical keys.
function tapKeys(btn) {
  if (!uc || runBtn.disabled) return;
  const action = btn.dataset.action;
  if (action === 'enter') {
    draw(guestKey(13));
  } else if (action === 'bs') {
    if (term.textContent.length > 0) term.textContent = term.textContent.slice(0, -1);
    draw(guestKey(0x7f));
  } else {
    for (const ch of btn.dataset.keys) draw(guestKey(ch.charCodeAt(0)));
  }
  term.focus();
}

async function run() {
  runBtn.disabled = true;
  term.textContent = '';
  stats = { steps: 0, insns: 0, emuMs: 0, chars: 0, wallStart: performance.now() };
  statsEl.textContent = '';
  try {
    const MUnicorn = window.MUnicorn;
    if (!MUnicorn) throw new Error('unicorn.js failed to load (check public/unicorn.js)');
    ucMod = await MUnicorn();
    board = await loadBoard();
    uc = new ucMod.Unicorn(ucMod.ARCH_ARM64, ucMod.MODE_LITTLE_ENDIAN);
    boot(ucMod, uc, board);

    draw(guestSlice()); // boot greeting + prompt

    setStatus('booted (AArch64 guest kernel) — type a command: HI, RPI, HELP');
    runBtn.textContent = 'Reboot';
    runBtn.disabled = false;
    term.focus();
    hint.textContent = '';
  } catch (err) {
    setStatus('ERROR: ' + err.message);
    console.error(err);
    runBtn.disabled = false;
  }
}

window.addEventListener('keydown', handleKey);
term.addEventListener('click', () => term.focus());
document.querySelectorAll('.osk button').forEach((btn) =>
  btn.addEventListener('click', () => tapKeys(btn))
);
window.addEventListener('error', (e) => {
  setStatus('ERROR: ' + (e.message || e.type));
});
runBtn.addEventListener('click', run);
run();
