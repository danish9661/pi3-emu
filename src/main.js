import './styles.css';

const UART_WINDOW = 0x1000;
const TX_SLOT_STRIDE = 4;
const TX_SLOTS = 8;
const RAM_BASE = 0x0;
const RAM_SIZE = 0x400000;
const KERNEL_ADDR = 0x80000;
const INSTRUCTIONS_PER_CHUNK = 16;

const term = document.getElementById('term');
const status = document.getElementById('status');
const runBtn = document.getElementById('run');

function setStatus(text) {
  status.textContent = text;
}

function draw(text) {
  term.textContent += text;
  term.scrollTop = term.scrollHeight;
  return text.length;
}

async function loadBoard() {
  const resp = await fetch('/pi_board.wasm');
  const { instance } = await WebAssembly.instantiate(await resp.arrayBuffer(), {});
  return instance.exports;
}

function boot(ucMod, uc, board) {
  const uart = Number(board.pi_uart_base());

  uc.mem_map(RAM_BASE, RAM_SIZE, ucMod.PROT_ALL);

  // DEVICE NOTES:
  // The PL011-style UART is mapped as plain memory for now. The guest kernel
  // appends TX characters as word-aligned slots in the device window, and the
  // host drains them between emulation chunks (uc_mem_read / uc_mem_write).
  // This sidesteps three bugs in this unicorn.js build:
  //   1. HOOK_MEM_* callbacks crash emulation ("memory access out of bounds")
  //   2. unaligned guest stores to RAM are dropped
  //   3. `and rN, rN, #imm` clobbers the destination register
  // Device registers (real PL011 registers, GPIO, ...) can later be layered
  // onto the same window by treating those words as device state.
  uc.mem_map(uart, UART_WINDOW, ucMod.PROT_READ | ucMod.PROT_WRITE);
  uc.devUart = { base: uart };

  const kptr = Number(board.pi_kernel());
  const klen = Number(board.pi_kernel_len());
  const kernel = new Uint8Array(board.memory.buffer, kptr, klen);
  for (let i = 0; i < klen; i++) {
    uc.mem_write(KERNEL_ADDR + i, [kernel[i]]);
  }

  uc.reg_write_i32(ucMod.ARM_REG_PC, KERNEL_ADDR);
  uc.reg_write_i32(ucMod.ARM_REG_SP, RAM_BASE + RAM_SIZE - 16);
}

// Between chunks: pull TX characters out of the UART device window into the
// board's console FIFO, then blank the consumed slots.
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
  return draw(out);
}

async function run() {
  runBtn.disabled = true;
  term.textContent = '';
  try {
    const MUnicorn = window.MUnicorn;
    if (!MUnicorn) throw new Error('unicorn.js failed to load (check public/unicorn.js)');
    const ucMod = await MUnicorn();
    const board = await loadBoard();
    const uc = new ucMod.Unicorn(ucMod.ARCH_ARM, ucMod.MODE_ARM);
    boot(ucMod, uc, board);

    setStatus('running (ARM32 core; aarch64 core is broken in unicorn.js)...');
    const t0 = performance.now();
    let totalInstructions = 0;
    let quietChunks = 0;
    let pc = KERNEL_ADDR;

    const step = () => {
      // chunked stepping via the instruction counter (the aarch64 `begin`
      // and HOOK_CODE-budget approaches don't stop at exact instruction
      // boundaries in this build; `count` does).
      uc.emu_start(pc, 0, 0, INSTRUCTIONS_PER_CHUNK);
      pc = Number(uc.reg_read_i32(ucMod.ARM_REG_PC));
      totalInstructions += INSTRUCTIONS_PER_CHUNK;
      const n = pumpUart(ucMod, uc, board);
      quietChunks = n > 0 ? 0 : quietChunks + 1;
      const charCount = drain(board);
      const elapsed = performance.now() - t0;
      if (quietChunks >= 4 && charCount > 0) {
        const ips = Math.round((totalInstructions / elapsed) * 1000);
        setStatus(`kernel halted (b .) — ${totalInstructions.toLocaleString()} instructions in ${Math.round(elapsed)} ms ≈ ${ips.toLocaleString()} ips (ARM32)`);
        runBtn.disabled = false;
        return;
      }
      requestAnimationFrame(step);
    };

    step();
  } catch (err) {
    setStatus('ERROR: ' + err.message);
    console.error(err);
    runBtn.disabled = false;
  }
}

runBtn.addEventListener('click', run);
run();