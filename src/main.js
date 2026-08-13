import './styles.css';
import { parseElf, loadElf } from './elf.js';

const UART_WINDOW = 0x1000;
const TX_SLOT_STRIDE = 4;
const TX_SLOTS = 32;
const RAM_BASE = 0x0;
const RAM_SIZE = 0x400000;
const SLICE_INSNS = 512;
const MAX_SLICES = 5000;

export const PROGRAMS = {
  shell: 'shell.elf',
  sum: 'sum.elf',
  fib: 'fib.elf',
};

const term = document.getElementById('term');
const status = document.getElementById('status');
const runBtn = document.getElementById('run');
const progSel = document.getElementById('prog');
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

// The programs are static bare-metal AArch64 ELFs; the host maps RAM + the
// UART window and loads the ELF segments at their vaddrs. The CPU starts at
// e_entry (passed to emu_start — reg_write is a no-op in this unicorn build)
// and the guest's own _start sets SP, so the host never touches registers.
// After that the guest runs freely: each slice is SLICE_INSNS of real
// AArch64 instructions, resuming from the current PC.
function boot(ucMod, uc, board, elf) {
  const uart = Number(board.pi_uart_base());
  rxSlot = uart + Number(board.pi_rx_offset());

  uc.mem_map(RAM_BASE, RAM_SIZE, ucMod.PROT_ALL);
  uc.mem_map(uart, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.devUart = { base: uart };
  uc.entry = elf.entry;

  loadElf(uc, elf);
}

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

function runSlice(count) {
  const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || uc.entry;
  const t0 = performance.now();
  uc.emu_start(pc, 0, 0, count);
  stats.emuMs += performance.now() - t0;
  stats.steps += 1;
  stats.insns += count;
  pumpUart(ucMod, uc, board);
  return drain(board);
}

function updateStats() {
  const pc = Number(uc.reg_read_i32(ucMod.ARM64_REG_PC)) || uc.entry;
  const sp = Number(uc.reg_read_i32(ucMod.ARM64_REG_SP));
  const wall = (performance.now() - stats.wallStart) / 1000;
  const mips = stats.emuMs > 0 ? (stats.insns / stats.emuMs / 1000).toFixed(2) : '—';
  const pcs = (pc - 0x100000).toString(16).padStart(6, '0');
  statsEl.innerHTML =
    `<span><span class="k">pc</span> 0x100000+0x${pcs}</span>` +
    `<span><span class="k">sp</span> 0x${sp.toString(16)}</span>` +
    `<span><span class="k">mips</span> ${mips}</span>` +
    `<span><span class="k">steps</span> ${stats.steps}</span>` +
    `<span><span class="k">insns</span> ${stats.insns}</span>` +
    `<span><span class="k">emu</span> ${stats.emuMs.toFixed(2)}ms</span>` +
    `<span><span class="k">wall</span> ${wall.toFixed(2)}s</span>` +
    `<span><span class="k">chars</span> ${stats.chars}</span>`;
}

// The guest drives itself: it prints to the UART TX slots (one char per
// slice) and parks in getc until a key arrives. Run slices until the guest
// has gone quiet for two consecutive slices — i.e. it is back waiting for
// input (or finished all its work).
function runUntilIdle() {
  let out = '';
  let quiet = 0;
  for (let i = 0; i < MAX_SLICES; i++) {
    const o = runSlice(SLICE_INSNS);
    out += o;
    updateStats();
    if (o === '') {
      quiet++;
      if (quiet >= 2) break;
    } else {
      quiet = 0;
    }
  }
  return out;
}

function guestKey(code) {
  uc.mem_write(rxSlot, [code]);
  return runUntilIdle();
}

function handleKey(e) {
  if (!uc || runBtn.disabled) return;
  if (e.key === 'Backspace') {
    e.preventDefault();
    if (term.textContent.length > 0) {
      term.textContent = term.textContent.slice(0, -1);
    }
    draw(guestKey(0x7f)); // guest unwrites its own line buffer
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

    const name = PROGRAMS[progSel.value];
    const resp = await fetch('./programs/' + name);
    if (!resp.ok) throw new Error('cannot fetch ./programs/' + name);
    const elf = parseElf(new Uint8Array(await resp.arrayBuffer()));
    boot(ucMod, uc, board, elf);

    draw(runUntilIdle()); // program boots, prints its banner, parks on getc

    setStatus(`booted — running ${name} (AArch64 ELF at 0x100000) — type, or press Reboot`);
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