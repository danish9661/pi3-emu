import './styles.css';

// Pi 3 (BCM2837) emulator demo — pi-cpu core (cpu/, Rust -> wasm) as the
// only CPU. All device models live inside the core's Bus; the page only
// drives slices, keys/buttons, the LED/canvas/audio panels, and the SD
// card Save/Load path. The Linux tab is untouched (qemu-wasm iframe).

const SLICE_INSNS = 4096;
const PWM_SLICE = 512; // FIFO depth 256: 4096-insn chunks overrun it (drops ~86%)
const SMP_SLICE = 512;
const SMP_MAX_ROUNDS = 2000;
const SMP_BUDGET = 20000000;
const MAX_SLICES = 5000;
const DONE_SLICES = 30000; // safety cap for the explicit-done guests
const FB_ADDR = 0x200000; // allocated framebuffer inside guest RAM

const LINUX_MODE = 'linux';
const LINUX_ST_MODE = 'linux-st';
const SMP_MODE = 'smp';
const CLOCK_MODE = 'clock';
const GPIO_MODE = 'gpio';
const FB_MODE = 'fb';
const IRQ_MODE = 'irq';
const LIRQ_MODE = 'lirq';
const UPY_MODE = 'upython';

// Explicit-done selector for PiEmu.done(): 0 = clock/gpio (TMR+0x20),
// 1 = mmu, 2 = dma, 3 = pwm, 4 = i2c, 5 = spi, 6 = sd (all +0x54),
// 7 = periphs/debug (USB DONE: +0xFF0 or +0x54).
const DONE_SEL = {
  clock: 0, gpio: 0, mmu: 1, dma: 2, pwm: 3, i2c: 4, spi: 5, sd: 6,
  periphs: 7, debug: 7,
};

const GPIO_LEDS = [21, 22, 23, 24, 25, 26, 27, 28];
const GPIO_BTN = 29;

export const PROGRAMS = {
  shell: 'shell.elf',
  sum: 'sum.elf',
  fib: 'fib.elf',
  smp: 'smp.elf',
  clock: 'clock.elf',
  gpio: 'gpio.elf',
  fb: 'fb.elf',
  irq: 'irq.elf',
  mmu: 'mmu.elf',
  dma: 'dma.elf',
  pwm: 'pwm.elf',
  i2c: 'i2c.elf',
  spi: 'spi.elf',
  uart1: 'uart1.elf',
  sd: 'sd.elf',
  uart0: 'uart0.elf',
  lirq: 'lirq.elf',
  upython: 'firmware.elf',
  rpikernel: 'rpi-kernel.elf',
  periphs: 'periphs.elf',
  debug: 'debug.elf',
  bench: 'bench.elf',
};

const term = document.getElementById('term');
const status = document.getElementById('status');
const runBtn = document.getElementById('run');
const progSel = document.getElementById('prog');
const linuxConfigSel = document.getElementById('linuxConfig');
const linuxThreadsSel = document.getElementById('linuxThreads');
try {
  if (window.SabToggle && linuxThreadsSel) {
    window.SabToggle.bindSelect(linuxThreadsSel);
  }
} catch (_) {}
function syncLinuxSels() {
  const show = (progSel.value === LINUX_MODE || progSel.value === LINUX_ST_MODE) ? 'inline-block' : 'none';
  if (linuxConfigSel) linuxConfigSel.style.display = show;
  if (linuxThreadsSel) linuxThreadsSel.style.display = show;
}
if (progSel && linuxConfigSel) {
  syncLinuxSels();
  progSel.addEventListener('change', syncLinuxSels);
}
const statsEl = document.getElementById('stats');
const hint = document.getElementById('hint');
const gpioPanel = document.getElementById('gpiopanel');
const fbCanvas = document.getElementById('fbscreen');
const cardbar = document.getElementById('cardbar');
const fbCtx = fbCanvas.getContext('2d');
const gpioLedsEl = document.getElementById('gpio-leds');
const gpioBtnEl = document.getElementById('gpio-btn');

// ---- MicroPython SD card persistence (Save/Load/Reset + IndexedDB) ----
const idbCardOpen = () => new Promise((res, rej) => {
  const r = indexedDB.open('pi3emu', 1);
  r.onupgradeneeded = () => {
    if (!r.result.objectStoreNames.contains('disk')) r.result.createObjectStore('disk');
  };
  r.onsuccess = () => res(r.result);
  r.onerror = () => rej(r.error);
});
const idbCardGet = async () => {
  const db = await idbCardOpen();
  return await new Promise((res, rej) => {
    const rq = db.transaction('disk', 'readonly').objectStore('disk').get('upycard');
    rq.onsuccess = () => res(rq.result || null);
    rq.onerror = () => rej(rq.error);
  });
};
const idbCardPut = async (buf) => {
  const db = await idbCardOpen();
  await new Promise((res) => {
    const tx = db.transaction('disk', 'readwrite');
    tx.objectStore('disk').put(new Uint8Array(buf), 'upycard');
    tx.oncomplete = res;
  });
};
const idbCardDel = async () => {
  const db = await idbCardOpen();
  await new Promise((res) => {
    const tx = db.transaction('disk', 'readwrite');
    tx.objectStore('disk').delete('upycard');
    tx.oncomplete = res;
  });
};
const downloadCard = (buf, name) => {
  const blob = new Blob([buf], { type: 'application/octet-stream' });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob);
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 5000);
};

// ---- pi-cpu wasm core (built by build.sh into public/pi_cpu/) ----
let PiEmu = null;
let PiSmp = null;
async function loadCore() {
  if (PiEmu) return;
  const base = (import.meta.env && import.meta.env.BASE_URL) || './';
  const url = new URL(base + 'pi_cpu/pi_cpu.js', location.href).href;
  const mod = await import(/* @vite-ignore */ url);
  await mod.default();
  PiEmu = mod.PiEmu;
  PiSmp = mod.PiSmp;
}

// ---- emulator state ----
let pi = null; // PiEmu: the single-core session
let smp = null; // PiSmp: the quad-core session
let mode = 'single';
let gpioBtn = 0;
let gpioLedEls = null;
let lastFault = null;
let lastFaultText = null;
let faultStreak = 0;
let lastWall = 0; // performance.now() at the last slice (wall_tick deltas)
let gpioLoopActive = false;
let gpioFrame = 0;
let fbFrame = 0;
let irqFrame = 0;
let stats = { steps: 0, insns: 0, emuMs: 0, chars: 0, wallStart: 0 };
let pwmFedTotal = 0; // drained PWM samples this boot (status + audio ring)
const textDec = new TextDecoder();

// Virtual time (?vt=1): the system timer advances per executed instruction
// (262144 ips — integer-exact 15625 us per 4096-slice) instead of wall
// clock — deterministic runs, instant sleeps.
const VIRTUAL_TIME = (() => {
  try { return new URLSearchParams(location.search).get('vt') === '1'; } catch (_) { return false; }
})();
const VIRTUAL_IPS = 262144;

function setStatus(text) {
  status.textContent = text;
}

function draw(text) {
  term.textContent += text;
  term.scrollTop = term.scrollHeight;
}

function takeConsole() {
  if (!pi) return '';
  const bytes = pi.take_console();
  if (!bytes || bytes.length === 0) return '';
  stats.chars += bytes.length;
  return textDec.decode(bytes);
}

function faultHalted() {
  if (faultStreak > 50 && lastFaultText) return 'guest fault: ' + lastFaultText;
  return null;
}

// One slice on the core: wall-clock tick (unless virtual time), run,
// console drain, audio feed, fault tracking. Returns console text.
function runSlice(count) {
  const t0 = performance.now();
  if (!VIRTUAL_TIME) {
    const now = performance.now();
    const dus = Math.max(0, Math.floor((now - lastWall) * 1000));
    lastWall = now;
    pi.wall_tick(dus);
  }
  try {
    pi.run(count);
    faultStreak = 0;
  } catch (e) {
    lastFault = e;
    try { lastFaultText = pi.fault() || String(e).slice(0, 120); } catch (_) { lastFaultText = String(e).slice(0, 120); }
    faultStreak++;
    try { window.__lastFault = lastFaultText; } catch (_) {}
  }
  stats.emuMs += performance.now() - t0;
  stats.steps += 1;
  try {
    const n = pi.insns();
    if (Number.isFinite(n)) stats.insns = n;
    else stats.insns += count;
  } catch (_) { stats.insns += count; }
  // PWM audio: drain whatever the chunk produced into the worklet/ring.
  try {
    const samples = pi.pwm_take(65536);
    pwmFedTotal += samples.length;
    for (let i = 0; i < samples.length; i++) audioPush(samples[i]);
  } catch (_) {}
  audioFlush();
  return takeConsole();
}

// ---- audio (WebAudio worklet with ScriptProcessor fallback) ----
let audioCtx = null;
let audioWorkletNode = null;
let audioUseWorklet = false;
let audioRing = null;
let audioPos = 0;
let audioLen = 0;
let audioPending = [];
function initAudio() {
  if (audioCtx) return;
  const AC = window.AudioContext || window.webkitAudioContext;
  if (!AC) return;
  audioCtx = new AC();
  audioRing = new Float32Array(1 << 18);
  window.__pwmEngine = 'script';
  const startScriptFallback = () => {
    if (audioWorkletNode || window.__pwmEngine === 'script-live') return;
    const sp = audioCtx.createScriptProcessor(4096, 0, 1);
    sp.onaudioprocess = (e) => {
      const out = e.outputBuffer.getChannelData(0);
      for (let i = 0; i < out.length; i++) {
        out[i] = audioLen > 0 ? audioRing[(audioPos++) & (audioRing.length - 1)] * 0.3 : 0;
        if (audioLen > 0) audioLen--;
      }
    };
    sp.connect(audioCtx.destination);
    window.__pwmEngine = 'script-live';
  };
  if (audioCtx.audioWorklet) {
    audioCtx.audioWorklet.addModule('./audio-pwm.js').then(() => {
      audioWorkletNode = new AudioWorkletNode(audioCtx, 'pi3-pwm');
      audioWorkletNode.connect(audioCtx.destination);
      if (audioLen > 0) {
        const backlog = new Float32Array(audioLen);
        for (let i = 0; i < audioLen; i++) backlog[i] = audioRing[(audioPos + i) & (audioRing.length - 1)];
        audioPos = 0;
        audioLen = 0;
        const n = backlog.length; // read BEFORE post: transfer neuters the buffer (length -> 0)
        try {
          audioWorkletNode.port.postMessage(backlog, [backlog.buffer]);
        } catch (_) {
          try { audioWorkletNode.port.postMessage(new Float32Array(n)); } catch (_) {}
        }
        window.__pwmFed = (window.__pwmFed || 0) + n;
      }
      audioUseWorklet = true;
      window.__pwmEngine = 'worklet';
    }).catch(() => startScriptFallback());
  } else {
    startScriptFallback();
  }
  if (audioCtx.state === 'suspended') audioCtx.resume();
}
function audioPush(sampleWord) {
  const s16 = ((sampleWord & 0xffff) << 16) >> 16; // low 16 bits = signed sample
  if (audioUseWorklet && audioWorkletNode) {
    if (audioPending.length < (1 << 19)) audioPending.push(s16 / 32768);
    return;
  }
  if (!audioRing) return;
  if (audioLen < audioRing.length) {
    audioRing[(audioPos + audioLen) & (audioRing.length - 1)] = s16 / 32768;
    audioLen++;
  }
}
function audioFlush() {
  if (!audioWorkletNode || audioPending.length === 0) return;
  const batch = new Float32Array(audioPending);
  audioPending = [];
  const n = batch.length; // read BEFORE post: transfer neuters the buffer
  try {
    audioWorkletNode.port.postMessage(batch, [batch.buffer]);
  } catch (_) {
    try { audioWorkletNode.port.postMessage(new Float32Array(batch)); } catch (_) {}
  }
  window.__pwmFed = (window.__pwmFed || 0) + n;
}

// ---- boot ----
async function bootProg(name, { slice = SLICE_INSNS } = {}) {
  await loadCore();
  const resp = await fetch('./programs/' + name);
  if (!resp.ok) throw new Error('cannot fetch ./programs/' + name);
  const bytes = new Uint8Array(await resp.arrayBuffer());
  pi = new PiEmu();
  pi.load_elf(bytes);
  pi.set_slice(slice);
  if (VIRTUAL_TIME) pi.set_vt_ips(VIRTUAL_IPS);
  lastWall = performance.now();
  lastFault = null;
  lastFaultText = null;
  faultStreak = 0;
  gpioBtn = 0;
  try { window.__lastFault = null; } catch (_) {}
  return bytes;
}

// ---- stats ----
function updateStats() {
  const wall = (performance.now() - stats.wallStart) / 1000;
  const mips = stats.emuMs > 0 ? (stats.insns / stats.emuMs / 1000).toFixed(2) : '—';
  let row = '';
  if (mode === SMP_MODE && smp) {
    try {
      const st = smp.state(); // [park, counter, msg0..3]
      row = `<span><span class="k">park</span> 0x${st[0].toString(16)}</span>` +
        `<span><span class="k">counter</span> ${st[1]}</span>` +
        `<span><span class="k">msg</span> ${st[2]} ${st[3]} ${st[4]} ${st[5]}</span>`;
    } catch (_) { row = '<span>smp</span>'; }
    try {
      const n = smp.insns();
      if (Number.isFinite(n)) stats.insns = n;
    } catch (_) {}
  } else if (pi) {
    let pc = 0, sp = 0;
    try { pc = pi.pc() >>> 0; } catch (_) {}
    try { sp = pi.sp() >>> 0; } catch (_) {}
    const rel = (pc - 0x100000) >>> 0;
    row = `<span><span class="k">pc</span> 0x100000+0x${rel.toString(16).padStart(6, '0')}</span>` +
      `<span><span class="k">sp</span> 0x${sp.toString(16)}</span>`;
  }
  statsEl.innerHTML =
    row +
    `<span><span class="k">mips</span> ${mips}</span>` +
    `<span><span class="k">steps</span> ${stats.steps}</span>` +
    `<span><span class="k">insns</span> ${stats.insns}</span>` +
    `<span><span class="k">emu</span> ${stats.emuMs.toFixed(2)}ms</span>` +
    `<span><span class="k">wall</span> ${wall.toFixed(2)}s</span>` +
    `<span><span class="k">chars</span> ${stats.chars}</span>`;
}

// ---- synchronous completion loops ----
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

// M54 kernel POST: fixed virtual-time budget, not TX-silence — the
// kernel spends megainsns between prints (SVC→MMU→3×1s timer ticks)
// and runUntilIdle's 2-quiet-slice break would stop 8k insns in,
// inside the SVC glue, leaving #term empty. Mirrors the native smoke
// budget (20M insns @ 4096 slices); keys typed later are picked up by
// the sync rAF loops (irqRun path below handles rpikernel like upython).
function runKernelPost() {
  let out = '';
  // ~20M insns; slice loop breaks early on guest fault (runSlice
  // catches into faultStreak — stop feeding a faulted core).
  for (let i = 0; i < 5000; i++) {
    out += runSlice(SLICE_INSNS);
    updateStats();
    const halted = faultHalted();
    if (halted) break;
    // Early exit once the echo loop is up (saves ~seconds of spinning:
    // the POST prints Echoing input now right before parking on getc).
    if (out.includes('Echoing input now')) break;
  }
  return out;
}

// Explicit-done guests (clock/gpio/mmu/dma/pwm/i2c/spi/sd): slices until
// the guest writes its DONE cell (polled via PiEmu.done()).
function runUntilDone(prog) {
  const sel = DONE_SEL[prog];
  const slice = prog === 'pwm' ? PWM_SLICE : SLICE_INSNS;
  let out = '';
  let done = false;
  for (let i = 0; i < DONE_SLICES; i++) {
    out += runSlice(slice);
    updateStats();
    try { done = pi.done(sel) !== 0; } catch (_) { done = false; }
    if (done) break;
    const halted = faultHalted();
    if (halted) break;
  }
  if (!done) {
    try { done = pi.done(sel) !== 0; } catch (_) {}
  }
  if (!done && !faultHalted()) setStatus('warn: guest did not reach DONE');
  return out;
}

// ---- gpio panel ----
function updateGpioPanel() {
  if ((mode !== GPIO_MODE && mode !== UPY_MODE) || !gpioPanel || !pi) return;
  if (mode !== GPIO_MODE) return; // LEDs only meaningful for the gpio guest
  if (!gpioLedEls) {
    gpioLedEls = [];
    for (const p of GPIO_LEDS) {
      const el = document.createElement('span');
      el.className = 'led';
      el.title = 'GPIO ' + p;
      gpioLedsEl.appendChild(el);
      gpioLedEls.push(el);
    }
  }
  let lev = 0;
  try { lev = pi.gpio_lev() >>> 0; } catch (_) {}
  for (let i = 0; i < GPIO_LEDS.length; i++) {
    gpioLedEls[i].classList.toggle('on', (lev & (1 << GPIO_LEDS[i])) !== 0);
  }
}

// The gpio chase is paced in animation frames: slices run for ~16 ms of wall
// time per frame, so the browser paints the LED panel between frames and the
// knight-rider chase is actually visible.
function gpioRun() {
  let out = '';
  gpioLoopActive = true;
  const frame = () => {
    const t0 = performance.now();
    do {
      out += runSlice(SLICE_INSNS);
      updateGpioPanel();
      let done = false;
      try { done = pi.done(DONE_SEL.gpio) !== 0; } catch (_) {}
      if (done) break;
    } while (performance.now() - t0 < 16);
    draw(out);
    out = '';
    updateStats();
    const halted = faultHalted();
    if (halted) {
      gpioLoopActive = false;
      setStatus(halted);
      return;
    }
    let done = false;
    try { done = pi.done(DONE_SEL.gpio) !== 0; } catch (_) {}
    if (done) {
      gpioLoopActive = false;
      setStatus('booted — running gpio — GPIO @ 0x3F200000 — chase done — hold BTN 29 to press');
      return;
    }
    gpioFrame = requestAnimationFrame(frame);
  };
  gpioFrame = requestAnimationFrame(frame);
}

// The fb guest animates forever and never parks, so a rAF loop advances
// slices (~16 ms of wall time per frame) and blits the framebuffer to the
// canvas right after each batch — the display runs live until Reboot.
function fbRun() {
  let out = '';
  const frame = () => {
    if (mode !== FB_MODE) return;
    const t0 = performance.now();
    do {
      out += runSlice(SLICE_INSNS);
    } while (performance.now() - t0 < 16);
    draw(out);
    out = '';
    updateStats();
    const halted = faultHalted();
    if (halted) {
      setStatus(halted);
      return;
    }
    blit();
    fbFrame = requestAnimationFrame(frame);
  };
  fbFrame = requestAnimationFrame(frame);
}

function blit() {
  if (!fbCtx || !pi) return;
  let w = 0, h = 0, ready = 0;
  try {
    const info = pi.fb_info();
    w = info[0]; h = info[1]; ready = info[3];
  } catch (_) { return; }
  if (!ready || w === 0 || h === 0) return;
  if (fbCanvas.width !== w || fbCanvas.height !== h) {
    fbCanvas.width = w;
    fbCanvas.height = h;
  }
  const n = w * h * 4;
  const img = fbCtx.createImageData(w, h);
  // PiEmu.mem_read caps a single read at 4 KB — assemble the frame in
  // chunks (19 reads for 160x120x32; cheap at rAF pace).
  let off = 0;
  try {
    while (off < n) {
      const want = Math.min(4096, n - off);
      const mem = pi.mem_read(FB_ADDR + off, want);
      if (!mem || mem.length === 0) return;
      img.data.set(mem.subarray(0, Math.min(mem.length, n - off)), off);
      if (mem.length < want) break;
      off += mem.length;
    }
  } catch (_) { return; }
  if (off === 0) return;
  for (let i = 3; i < img.data.length; i += 4) img.data[i] = 255; // opaque
  fbCtx.putImageData(img, 0, 0);
}

// The irq, lirq, upython and rpikernel guests never park (infinite spin
// with IRQs unmasked), so they run on rAF slices; IRQ delivery happens
// inside the core at chunk boundaries (host-assisted IRQ_RET resume or
// native eret). rpikernel joins this loop after runKernelPost so typed
// keys reach its echo loop (handleKey's rAF path pushes directly).
function irqRun() {
  let out = '';
  const frame = () => {
    if (mode !== IRQ_MODE && mode !== LIRQ_MODE && mode !== UPY_MODE && mode !== 'rpikernel') return;
    const t0 = performance.now();
    do {
      out += runSlice(SLICE_INSNS);
    } while (performance.now() - t0 < 16);
    draw(out);
    out = '';
    updateStats();
    const halted = faultHalted();
    if (halted) {
      setStatus(halted);
      return;
    }
    irqFrame = requestAnimationFrame(frame);
  };
  irqFrame = requestAnimationFrame(frame);
}

// SMP: one PiSmp session runs all four cores round-robin until every core
// parks (or the round/budget cap). Synchronous — the whole join takes a
// fraction of a second at pi-cpu speed.
function smpRunSync() {
  smp.run(SMP_SLICE, SMP_MAX_ROUNDS, SMP_BUDGET);
  let out = '';
  try {
    const bytes = smp.take_console();
    if (bytes && bytes.length) {
      stats.chars += bytes.length;
      out = textDec.decode(bytes);
    }
  } catch (_) {}
  try {
    const n = smp.insns();
    if (Number.isFinite(n)) stats.insns = n;
  } catch (_) {}
  return out;
}

// Linux never idles, so it runs with the same fixed-budget frame loop as
// the IRQ guests; IRQs are delivered natively (CPU_INTERRUPT_HARD via the
// local block, real vectors, real eret) with the arch timer ticked per
// slice, exactly like LIRQ_MODE. The boot runs until the user reboots.
// Linux boot uses the qemu-wasm engine (a self-contained page at
// /linux/index.html) instead of the pi-cpu core. We embed it in an iframe
// rather than driving it from JS: the page wires xterm to the emulated
// PL011 via xterm-pty and boots the raspi3ap machine (4x Cortex-A53, 512 MB)
// with the prebuilt kernel8.img + DTB + busybox rootfs.
function runLinux(base, forcedThreads) {
  base = base || './linux/index.html';
  cancelAnimationFrame(gpioFrame);
  cancelAnimationFrame(fbFrame);
  cancelAnimationFrame(irqFrame);
  gpioLoopActive = false;
  runBtn.disabled = true;
  term.hidden = true;
  gpioPanel.hidden = true;
  fbCanvas.hidden = true;
  const hintEl = document.getElementById('hint');
  const oskEl = document.getElementById('osk');
  if (hintEl) hintEl.hidden = true;
  if (oskEl) oskEl.hidden = true;
  const linuxBoot = document.getElementById('linuxBoot');
  if (linuxBoot) { linuxBoot.hidden = false; linuxBoot.textContent = 'booting Linux…'; }
  const cfg = linuxConfigSel ? linuxConfigSel.value : 'minimal';
  const threads = forcedThreads || (linuxThreadsSel ? linuxThreadsSel.value : 'auto');
  window.__linuxConfig = cfg;
  window.__linuxThreads = threads;
  const linuxUrl = base + '#cfg=' + encodeURIComponent(cfg) +
    '&threads=' + encodeURIComponent(threads);
  // Always boot a FRESH iframe: reusing one via src=/location assignment is
  // unreliable when only the hash changes (cfg/threads live in the hash) —
  // same-document fragment navigations don't reload, so the engine would keep
  // stale argv. Recreation also drops stale wasm workers from the last boot.
  let frame = document.getElementById('linuxframe');
  if (frame) frame.remove();
  frame = document.createElement('iframe');
  frame.id = 'linuxframe';
  frame.style.width = '100%';
  frame.style.height = '82vh';
  frame.style.border = '0';
  frame.style.background = '#111';
  term.parentNode.insertBefore(frame, term.nextSibling);
  frame.src = linuxUrl;
  frame.hidden = false;
  const engineTag = base.indexOf('linux-st') !== -1
    ? 'qemu-wasm raspi3ap single-thread engine (no SharedArrayBuffer needed)'
    : 'qemu-wasm raspi3ap';
  setStatus('booting Linux (' + cfg + ' / threads ' + threads + ') — ' + engineTag + ' — serial console in the frame below');
  hint.textContent = 'Linux runs in the embedded frame (threads: auto = MTTCG when isolated, else single-thread fallback). Press Reboot to reload the VM.';
  runBtn.textContent = 'Reboot';
  runBtn.disabled = false;
}

// Receive boot-phase updates from the Linux iframe (serial milestones) and
// surface them in the parent UI's #linuxBoot indicator.
window.addEventListener('message', (e) => {
  if (e.data && e.data.type === 'linux-boot') {
    const el = document.getElementById('linuxBoot');
    if (el && !el.hidden) el.textContent = 'Linux boot: ' + e.data.phase;
  }
});

// The guest drives itself: it prints to the UART TX slots (one char per
// slice) and parks in getc until a key arrives. Run slices until the guest
// has gone quiet for two consecutive slices — i.e. it is back waiting for
// input (or finished all its work).
function guestKey(code) {
  pi.push_key(code); // queue into the PL011 RX FIFO (guest's getc pops it)
  return runUntilIdle();
}

function handleKey(e) {
  if (!pi || runBtn.disabled) return;
  if (mode === IRQ_MODE || mode === LIRQ_MODE || mode === UPY_MODE || mode === 'rpikernel') {
    // Continuous rAF loop picks the key up at the next slice.
    const c = e.key.length === 1 ? e.key.charCodeAt(0) : e.key === 'Enter' ? 13 : 0;
    if (!c) return;
    e.preventDefault();
    try { pi.push_key(c); } catch (_) {}
    return;
  }
  if (mode === SMP_MODE || mode === FB_MODE) return;
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
  if (!pi || runBtn.disabled) return;
  if (mode === IRQ_MODE || mode === LIRQ_MODE || mode === UPY_MODE || mode === 'rpikernel') {
    const action = btn.dataset.action;
    const push = (c) => { try { pi.push_key(c); } catch (_) {} };
    if (action === 'enter') {
      push(13);
    } else if (action === 'bs') {
      push(0x7f);
    } else {
      for (const ch of btn.dataset.keys) push(ch.charCodeAt(0));
    }
    term.focus();
    return;
  }
  if (mode === SMP_MODE || mode === FB_MODE) return;
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
  // Linux boot is handled by the qemu-wasm engine in an embedded iframe,
  // not by the pi-cpu core — bail out before initializing anything.
  if (progSel.value === LINUX_MODE) {
    runLinux();
    return;
  }
  if (progSel.value === LINUX_ST_MODE) {
    // Direct boot of the dedicated single-thread engine (public/linux-st/,
    // initramfs boot, no SharedArrayBuffer needed). This bypasses the
    // sentinel-gated auto-handoff in public/linux/index.html: an explicit
    // user choice to try the ST engine. Deep ST execution is still the
    // upstream-blocked path (see README/M32 + 49a56a7); this option makes
    // the attempt observable in a real browser.
    runLinux('./linux-st/index.html', 'off');
    return;
  }
  cancelAnimationFrame(gpioFrame);
  cancelAnimationFrame(fbFrame);
  cancelAnimationFrame(irqFrame);
  gpioLoopActive = false;
  runBtn.disabled = true;
  term.hidden = false;
  const linuxFrameEl = document.getElementById('linuxframe');
  if (linuxFrameEl) linuxFrameEl.hidden = true;
  const linuxBootEl = document.getElementById('linuxBoot');
  if (linuxBootEl) linuxBootEl.hidden = true;
  term.textContent = '';
  gpioPanel.hidden = true;
  fbCanvas.hidden = true;
  cardbar.hidden = true;
  stats = { steps: 0, insns: 0, emuMs: 0, chars: 0, wallStart: performance.now() };
  pwmFedTotal = 0;
  statsEl.textContent = '';
  try {
    await loadCore();
    const sel = progSel.value;

    if (sel === SMP_MODE) {
      mode = SMP_MODE;
      const resp = await fetch('./programs/' + PROGRAMS.smp);
      if (!resp.ok) throw new Error('cannot fetch ./programs/' + PROGRAMS.smp);
      const bytes = new Uint8Array(await resp.arrayBuffer());
      smp = new PiSmp(bytes);
      pi = null;
      draw(smpRunSync()); // 4 cores round-robin until all park
      updateStats();
      setStatus(
        `booted — running smp — 4 AArch64 cores, mailbox @ 0x3F202000 — press Reboot to re-run`
      );
    } else if (sel === CLOCK_MODE) {
      mode = CLOCK_MODE;
      await bootProg(PROGRAMS.clock);
      draw(runUntilDone('clock')); // runs until the guest writes TMR_DONE
      setStatus(
        `booted — running clock — BCM system timer @ 0x3F003000 — press Reboot to re-run`
      );
    } else if (sel === GPIO_MODE) {
      mode = GPIO_MODE;
      gpioPanel.hidden = false;
      await bootProg(PROGRAMS.gpio);
      updateGpioPanel();
      gpioRun(); // async: rAF-paced slices, chase visibly blinks the LEDs
      setStatus(`booted — running gpio — GPIO @ 0x3F200000 — chase in progress`);
    } else if (sel === FB_MODE) {
      mode = FB_MODE;
      fbCanvas.hidden = false;
      await bootProg(PROGRAMS.fb);
      fbRun(); // async: rAF-paced slices + canvas blit every frame
      setStatus(
        `booted — running fb — framebuffer 160x120x32 @ 0x200000 via mailbox — live canvas`
      );
    } else if (sel === IRQ_MODE || sel === 'uart0') {
      mode = IRQ_MODE;
      await bootProg(sel === 'uart0' ? PROGRAMS.uart0 : PROGRAMS.irq);
      irqRun(); // async: rAF-paced slices, IRQs delivered inside the core
      setStatus(
        sel === 'uart0'
          ? `booted — running uart0 — BCM2837 PL011 @ 0x3F201000 — config verified, RXINTR -> IRQ 57 — type a key`
          : `booted — running irq — BCM2835 interrupt controller @ 0x3F00B200 — timer + PL011 IRQs live`
      );
    } else if (sel === LIRQ_MODE) {
      mode = LIRQ_MODE;
      await bootProg(PROGRAMS.lirq);
      irqRun(); // async: rAF-paced slices, real IRQs via the local block
      setStatus(
        `booted — running lirq — BCM2836 local interrupt block @ 0x40000000 — CNTPNS + GPU IRQ delivered to a real vector`
      );
    } else if (sel === UPY_MODE) {
      mode = UPY_MODE;
      await bootProg(PROGRAMS.upython);
      try {
        const saved = await idbCardGet();
        if (saved && saved.byteLength > 0 && pi.import_card(new Uint8Array(saved))) {
          setStatus('restored saved SD card image — booting upython');
        }
      } catch (e) { console.log('card restore failed: ' + e); }
      cardbar.hidden = false;
      gpioPanel.hidden = false; // BTN 29 doubles as the Pin.irq button
      irqRun(); // async: rAF-paced slices, REPL on the PL011 + real Pin.irq
      setStatus(
        `booted — running upython — MicroPython on BCM2837, /sd auto-mounted — type Python, card buttons below save the SD image`
      );
    } else if (sel === 'mmu') {
      mode = 'mmu';
      await bootProg(PROGRAMS.mmu);
      draw(runUntilDone('mmu')); // guest builds page tables, core translates natively
      setStatus(
        `booted — running mmu — host-assisted MMU @ 0x3F00D000 — press Reboot to re-run`
      );
    } else if (sel === 'dma') {
      mode = 'dma';
      await bootProg(PROGRAMS.dma);
      draw(runUntilDone('dma')); // core performs the 3-CB chain between slices
      setStatus(
        `booted — running dma — BCM2835 DMA @ 0x3F007000 — 3-CB chain + completion IRQ — press Reboot to re-run`
      );
    } else if (sel === 'pwm') {
      mode = 'pwm';
      await bootProg(PROGRAMS.pwm, { slice: PWM_SLICE });
      initAudio(); // the Run click is a user gesture: the melody can play
      draw(runUntilDone('pwm')); // FIFO-mode sample generation, paced by FULL1
      setStatus(
        `booted — running pwm — BCM2835 PWM @ 0x3F20C000 — ${pwmFedTotal} samples in the audio ring — press Reboot to re-run`
      );
    } else if (sel === 'i2c') {
      mode = 'i2c';
      await bootProg(PROGRAMS.i2c);
      draw(runUntilDone('i2c')); // sensor slave: WHO_AM_I, TEMP, COUNTER reads
      setStatus(
        `booted — running i2c — BCM2835 BSC master @ 0x3F804000 — sensor reads — press Reboot to re-run`
      );
    } else if (sel === 'spi') {
      mode = 'spi';
      await bootProg(PROGRAMS.spi);
      draw(runUntilDone('spi')); // flash slave answers the JEDEC ID, twice
      setStatus(
        `booted — running spi — BCM2835 SPI0 master @ 0x3F204000 — JEDEC ID — press Reboot to re-run`
      );
    } else if (sel === 'uart1') {
      mode = 'uart1';
      await bootProg(PROGRAMS.uart1);
      draw(runUntilIdle()); // second console: parks on getc like the shell
      setStatus(
        `booted — running uart1 — BCM2835 AUX mini UART @ 0x3F215000 — output tagged [u1] — press Reboot to re-run`
      );
    } else if (sel === 'rpikernel') {
      mode = 'rpikernel';
      await bootProg(PROGRAMS.rpikernel);
      // M54: the kernel idles on vt-driven timer ticks, not on TX
      // silence — runUntilIdle would break after 2 quiet slices (8k
      // insns), deep inside the SVC glue, and leave #term empty. Run
      // a virtual-time POST budget instead (same shape as the smoke
      // golden: 20M insns covers EL2→SVC→MMU→3 ticks; the echo loop
      // then stays live for typed keys via the sync rAF loops below).
      draw(runKernelPost());
      irqRun(); // keep the echo loop + timer IRQs live; keys via push_key
      setStatus(
        `booted — running rpikernel — own Rust kernel @ 0x80000, PL011 echo — type a key`
      );
    } else if (sel === 'sd') {
      mode = 'sd';
      await bootProg(PROGRAMS.sd);
      draw(runUntilDone('sd')); // FAT12 card: boot sector, root dir, HELLO.TXT
      setStatus(
        `booted — running sd — BCM2835 SDHCI (EMMC) @ 0x3F300000 — FAT12 card, HELLO.TXT read — press Reboot to re-run`
      );
    } else if (sel === 'periphs') {
      mode = 'periphs';
      await bootProg(PROGRAMS.periphs);
      draw(runUntilDone('periphs')); // M30 windows: parks on USB DONE
      setStatus(
        `booted — running periphs — M30 peripherals (RNG/clock/I2S/SPI1/USB/UART2-5) — press Reboot to re-run`
      );
    } else if (sel === 'debug') {
      mode = 'debug';
      await bootProg(PROGRAMS.debug);
      draw(runUntilDone('debug')); // diagnostic sweep: parks on USB DONE
      setStatus(
        `booted — running debug — diagnostic report across all windows — press Reboot to re-run`
      );
    } else {
      mode = 'single';
      const name = PROGRAMS[sel] || PROGRAMS.shell;
      await bootProg(name);
      draw(runUntilIdle()); // program boots, prints its banner, parks on getc
      setStatus(`booted — running ${name} (AArch64 ELF at 0x100000) — type, or press Reboot`);
    }
    runBtn.textContent = 'Reboot';
    runBtn.disabled = false;
    term.focus();
    hint.textContent = '';
    if (lastFaultText && faultStreak > 0) draw('\n[' + lastFaultText + ']\n');
  } catch (err) {
    setStatus('ERROR: ' + (err && (err.stack || err.message || err) || err));
    console.error(err);
    runBtn.disabled = false;
  }
}

// The GPIO button is a host-side input: while held, the host drives BTN 29
// high in GPLEV, and slices are resumed so the guest's poll loop sees it.
// (During the rAF-paced chase the frame loop advances the guest itself.)
function pressGpioBtn(down) {
  if (!pi || runBtn.disabled || (mode !== GPIO_MODE && mode !== UPY_MODE)) return;
  gpioBtn = down ? 1 : 0;
  try { pi.set_button(!!down); } catch (_) {}
  gpioBtnEl.classList.toggle('held', !!down);
  if (!gpioLoopActive && mode === GPIO_MODE) draw(runUntilIdle());
}

window.addEventListener('keydown', handleKey);
term.addEventListener('click', () => term.focus());
document.querySelectorAll('.osk button').forEach((btn) =>
  btn.addEventListener('click', () => tapKeys(btn))
);
gpioBtnEl.addEventListener('pointerdown', () => pressGpioBtn(true));
gpioBtnEl.addEventListener('pointerup', () => pressGpioBtn(false));
gpioBtnEl.addEventListener('pointerleave', () => pressGpioBtn(false));
window.addEventListener('error', (e) => {
  setStatus('ERROR: ' + (e.message || e.type));
});

runBtn.addEventListener('click', run);

// MicroPython card buttons (visible while upython runs; see #cardbar).
// Save snapshots the live card to IndexedDB + downloads it; Load reads a
// .bin file into IndexedDB and reboots (injected before boot); Reset
// clears the saved image and reboots pristine.
try {
  document.getElementById('saveCard').addEventListener('click', async () => {
    try {
      if (!pi) { setStatus('no SD card model (run upython first)'); return; }
      const buf = pi.export_card();
      await idbCardPut(buf);
      downloadCard(buf, 'pi3-card-' + Date.now() + '.bin');
      setStatus(`saved SD card (${buf.length} bytes) — also in IndexedDB`);
    } catch (e) { setStatus('Save Card failed: ' + (e && e.message || e)); }
  });
  const cardUpload = document.getElementById('cardUpload');
  document.getElementById('loadCard').addEventListener('click', () => cardUpload.click());
  cardUpload.addEventListener('change', () => {
    const f = cardUpload.files[0]; if (!f) return;
    const reader = new FileReader();
    reader.onload = async () => {
      try {
        await idbCardPut(new Uint8Array(reader.result));
      } catch (e) { setStatus('Load Card failed: ' + (e && e.message || e)); }
      cardUpload.value = '';
      run();
    };
    reader.readAsArrayBuffer(f);
  });
  document.getElementById('resetCard').addEventListener('click', async () => {
    try { await idbCardDel(); } catch (e) { console.log('card reset failed: ' + e); }
    run();
  });
} catch (_) {}
run();
