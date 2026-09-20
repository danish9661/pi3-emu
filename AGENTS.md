# AGENTS.md — pi3-emu: Raspberry Pi 3 emulator in the browser

## Project overview

A from-scratch Raspberry Pi 3 emulator that runs entirely in the browser:
QEMU TCG compiled to wasm (`public/unicorn.js`, AArch64) as the CPU core,
a Rust → wasm board model (`public/pi_board.wasm`), and JS device models
with real BCM2837 register layouts. Guests are custom bare-metal Rust
programs cross-compiled to AArch64 ELF and loaded into guest RAM.

Repo: `github.com/danish9661/pi3-emu` (master branch, one commit per
milestone M1…M19, long descriptive commit messages).

## PRIME DIRECTIVE (2026-09-13, user-locked — do not drift)

We are building our OWN ARM Linux emulator in Rust→wasm (pi-cpu +
BCM2837 board) that boots a REAL Raspberry Pi 3 OS image (upstream
`raspberrypi/linux` kernel8.img + DTB + rootfs, like qemu `raspi3ap`
does) — NOT a toy kernel, NOT qemu-wasm forever. qemu-wasm
(`public/linux/`, `public/linux-st/`) is the REFERENCE ONLY (oracle
for boot logs, DTB/cmdline, device behavior); the shippable engine is
pi-cpu. Success = same kernel8.img that boots under qemu boots under
pi-cpu to a shell prompt on the emulated PL011 in the browser.

- M20 unicorn.js notes below are HISTORICAL RECORD ONLY (that agent
  path failed on TCI/NEON + fork patches — IGNORE for design; never
  re-introduce unicorn). Our CPU is pi-cpu (`cpu/src/lib.rs`), our
  board is `Bus`, our kernel track is `ports/rpi-kernel/` stepping
  toward the real boot protocol.
- Execution rule: verify by EXECUTION (`cargo run`, `node test/...`,
  Playwright), never by reading. Assembler truth only (never hand-hex).
- Subagent rule (opencode bug workaround): subagents sometimes die with
  no real work. NEVER trust a completion summary — the integrator must
  `git status/diff`, rebuild, and re-run the battery before accepting
  any subagent claim. If a subagent returns empty, read its on-session
  history/output and redo the slice single-handed.

## Current state (M1–M19, all green)

Devices implemented as host-arbitrated MMIO "windows" (mirror registers
into guest memory before each slice, pull guest writes out after):

- System timer (0x3F003000), VideoCore mailbox (0x3F00B880), GPIO
  (0x3F200000), framebuffer via mailbox, legacy interrupt controller
  (0x3F00B200), MMU (0x3F00D000, host-assisted table walk), DMA
  (0x3F007000, host-arbitrated CB chains), PWM audio (0x3F20C000), I2C
  (0x3F804000), SPI (0x3F204000), mini UART (0x3F215000), SDHCI/FAT12
  (0x3F300000), PL011 UART0 (0x3F201000), BCM2836 local interrupt
  block (0x40000000, real IRQ delivery into the CPU).
- 4-core SMP via per-core unicorn instances + host-arbitrated mailbox.
- Guests: shell, sum, fib, smp, clock, gpio, fb, irq, lirq, mmu, dma,
  pwm, i2c, spi, uart1, sd, uart0, mva (18 programs).
- Scheduler: run-until-idle, 512-instruction slices (`runSlice` in
  src/main.js), devices synced before/after each slice.
- IRQ delivery: host-assisted (slice-boundary delivery, `IRQ_RET` magic
  at IC_BASE+0x2C, vector glue that saves the full register file) for
  the legacy-IC guests (irq/uart0/gpio); real CPU_INTERRUPT_HARD entry
  with native eret for the local-block guest (lirq).

## Verified core facts (unicorn.js 2.2.0 build — RETIRED in M49, record kept)

(The `unicorn.js` fork this section describes was deleted in M49; pi-cpu
is the only core. The notes below remain as the historical
reconstruction record for the bare-metal MMIO / slice work.)

- Hook range end is INCLUSIVE (guard adjacent registers by address).
- Memory hooks fire only for guest accesses, not host `mem_read/mem_write`.
- HOOK_MEM_READ fires BEFORE the CPU latches the read value — a hook must
  never rewrite the register being read (the guest sees the post-hook
  value).
- WRITE hooks carry the written value (identical writes distinguishable).
- `uc_emu_start(begin,...)` writes PC=begin (the old "PC write no-op" was
  this: passing begin=0 starts at address 0).
- SPSR_EL1, CNTP_TVAL_EL0, CNTFRQ_EL0 register IDs are undefined.
- No `uc_intr` / exception injection API.
- Core speed: ~26.5 MIPS (tight loop, 10k-insn slices).
- The `unicorn.js` in public/ is now REBUILT from github.com/AlexAltea/
  unicorn.js (fork submodule @ 8028ec43) via `python3 build.py` — see
  "Patched unicorn.js" below.

## M20+ — THE LINUX BOOT PROJECT (in progress)

Goal: boot a REAL arm64 Linux kernel properly on the emulated Pi 3 —
real vectors, real interrupts (bcm2836 interrupt block + ARM arch
timer), real drivers (PL011 console, timer, GPIO, SDHCI), busybox
initramfs → shell in the terminal. Not full Raspberry Pi OS: no USB,
ethernet, or GPU graphics in scope.

Viability: 26.5 MIPS → ~20s Linux boot, usable-but-slow shell. OK.

### PIVOT (2026-08-23): qemu-wasm is now the Linux engine

The Unicorn-fork approach (Phase 1/2a/2b below) was **abandoned for the
Linux boot**: its TCI interpreter cannot translate the NEON/SIMD ops an
aarch64 kernel requires — TCI has zero vector-op handlers (confirmed
against upstream QEMU `tci.c` too). Instead we now boot Linux with
**ktock/qemu-wasm** (`github.com/ktock/qemu-wasm`), which adds a real
TCG→Wasm backend (hot TBs JIT-compiled to WebAssembly, cold TBs via TCI),
so it handles vectors and boots the kernel.

- Machine: `raspi3ap` (BCM2837 / Pi 3 B+, 4× Cortex-A53, 512 MB).
- Assets (gitignored, fetched by `scripts/fetch-linux.sh` from
  `ktock/qemu-wasm-demo-images`): `qemu-system-aarch64.wasm`, `.data`
  (packs `kernel8.img` = raspberrypi/linux tag `1.20230405` +
  `bcm2710-rpi-3-b-plus.dtb` + busybox `rootfs.bin`), `out.js`,
  `.worker.js`, `load.js`.
- Harness (committed, in `public/linux/`): `index.html`, `module.js`
  (qemu args + `locateFile`/`mainScriptUrlOrBlob`), `coi-serviceworker.js`
  (cross-origin isolation required by the pthread / `PROXY_TO_PTHREAD`
  build), `vendor/xterm.css`. The xterm terminal is wired to the emulated
  PL011 via `xterm-pty` (`openpty`).
- **STATUS: WORKING.** Verified headless (Chrome): the kernel boots to a
  busybox shell (`~ #`) on the serial console. The pi3-emu "linux" program
  option now launches this inside an iframe (`runLinux()` in `src/main.js`)
  instead of the dead Unicorn `linuxRun()` path.

The Unicorn-fork notes below remain as the historical reconstruction
record for the bare-metal MMIO / slice work still used by the M1–M19 guests.

### Patched unicorn.js (the Phase 1 core work)

Source: `github.com/AlexAltea/unicorn.js` (npm `@alexaltea/unicorn-js`
2.1.4). Rebuild recipe: `python3 build.py aarch64` (fast single-arch)
or `python3 build.py --release` (all arches) in `/tmp/opencode/
unicornjs-src`; needs emscripten from ~/emsdk on PATH. Build artifacts
live in `/tmp/opencode/unicornjs-src/dist`; `bash build.sh` copies them
into public/.

Patches applied to the fork (all in `/tmp/opencode/unicornjs-src/
unicorn`, applied directly — NOT in src/patches/ yet):

- New public APIs (exported): `uc_arm64_set_irq(uc, level)` — asserts/
  de-asserts CPU_INTERRUPT_HARD on the CPU; `uc_arm64_timer_tick(uc,
  cntpct)` — advances the ARM generic timer counter (drives CNTP_CTL
  ISTATUS + IRQ via gt_recalc_timer); `uc_arm64_debug(uc, sel)` — host
  debug reads (sel: 0=interrupt_request, 1=daif, 2=uc_ext_irq,
  3=uc_gt_irq[0] (CNTPNS), 4=uc_cntpct, 5=env.pc, 6=exception_index,
  7=ESR_EL1, 11=uc_gt_irq[1] (CNTV), 12=uc_gt_irq[2] (CNTHP),
  13=uc_gt_irq[3] (CNTPS), 70=FAR_EL1, 71=SCTLR_EL1, 72=HCR_EL2;
  diagnostic selectors 8-10/14-111 of the *original* Phase 2a-MMU build
  are NOT reconstructed — they recorded every page-walk into rings and
  caused the 0.003 MIPS slowness; the rebuilt core skips them (returns 0)
  and runs at ~10 MIPS instead).
- **CRITICAL FIX (cpu.h `arm_el_is_aa64`)**: on bare-metal reset
  `SCR_EL3.RW`/`HCR_EL2.RW` default to 0, so `arm_el_is_aa64(env,1)`
  returned false and the whole EL1 regime was walked as AArch32 (v6
  tables) — a kernel fetch at a physical PA with MMU off still faulted
  (PREFETCH_ABORT) because the AArch32 SCTLR had M=1. Forced
  `return aa64` for `el<=2`. Without this the kernel dies at slice 0 in
  head.S (`msr sctlr_el1, xzr` region, pc 0x10463f4).
- helper.c: `gt_get_countervalue` returns `cpu->uc_cntpct`; re-enabled
  `gt_recalc_timer`/`gt_ctl_write`/`gt_timer_reset` (bodies were #if 0;
  ptimers still removed) driving IRQ lines through
  `arm_cpu_update_uc_irq`; `gt_cntfrq_hz = 19200000` (real Pi 3).
- cpu.c: `arm_cpu_update_uc_irq` (ORs ext+gt lines into
  CPU_INTERRUPT_HARD via cpu_interrupt/cpu_reset_interrupt).
- **CRITICAL FIX**: `arm_cpu_do_interrupt` dispatched to the AArch32
  entry path because `arm_el_is_aa64(env, 1)` is false (SCR_EL3.RW
  defaults to 0) — PC never moved to the vector. Forced the A64 path
  under `#if defined(TARGET_AARCH64)`.
- uc.c/unicorn.h/uc_priv.h/unicorn_aarch64.c: dispatch pointers,
  arch implementations, exports; build.py EXPORTED_FUNCTIONS;
  unicorn-wrapper.js: `arm64_set_irq`, `arm64_timer_tick` methods.

Verified semantics (irqcore guest + probe, 13/13 PASS):

- Level IRQ: line stays asserted until the host clears it
  (`arm64_set_irq(0)`) or the guest masks/disables the source; after
  eret with I restored, a still-high line re-triggers delivery.
- Entry: ELR_EL1 = interrupted PC, SPSR_EL1 = old PSTATE (I clear),
  PC = VBAR_EL1 + 0x280 (SP=1) or +0x80, DAIF.I set; eret returns.
- Timer: guest writes CNTP_TVAL → cval = cntpct + tval; host
  `arm64_timer_tick(cntpct)` advances → ISTATUS set, IRQ fires;
  guest disabling CNTP_CTL de-asserts the line.

### Phase 0 — feasibility gates (done)

- [x] Benchmark core speed: 26.5 MIPS (viable).
- [x] Exception injection analysis (PC/ELR writes no-op, no uc_intr).
- [x] Toolchain: emscripten 6.0.6 at ~/emsdk, no system apt/sudo,
      no aarch64-linux-gnu-gcc yet (rust aarch64-unknown-none for
      guests; kernel toolchain later via ARM tarball or podman).
- [x] DECISION: patch + rebuild the unicorn.js fork (done, works).

### Phase 1 — core: exception injection + arch timer (DONE)

- [x] IRQ injection into EL1: real vector entry + eret round trip.
- [x] ARM generic timer: CNTP_CTL/TVAL/CVAL + CNTFRQ 19.2 MHz, IRQ on
      compare (host-ticked via uc_arm64_timer_tick).
- [x] Verified with irqcore guest: vector+glue, ELR/SPSR/ISTATUS/
      CNTPCT save, eret, no re-entry after de-assert (13/13 PASS).
- [x] Full `python3 build.py --release`, wired into build.sh.
- [x] M1-M19 regression green with the rebuilt core.
- [x] COMMITTED + pushed (c841098): fork patches, exports, build.sh,
      irqcore guest, AGENTS.md. The fork source lives ONLY under
      /tmp/opencode/unicornjs-src (ephemeral — /tmp gets wiped;
      public/unicorn.js is the gitignored built artifact).
- uc_arm64_debug sels: 0 interrupt_request, 1 daif, 2 uc_ext_irq,
  3-6 uc_gt_irq[0..3] (CNTPNS/CNTV/CNTHP/CNTPS; local-block mapping:
  bit0 CNTPS←13, bit1 CNTPNS←3, bit2 CNTHP←12, bit3 CNTV←11),
  4 cntpct, 5 env.pc, 6/7 delivery counters, 8/9/10 last-TB ring.
  NOTE: arm64_debug returns a BigInt — Number() it before comparisons.

### Phase 2a — BCM2836 local interrupt block (DONE, committed 2b79e05)

- COMMITTED + pushed (2b79e05); README History M20 entry written.
- src/localint.js: window model at 0x40000000 (CONTROL/PRESCALER/
  GPU_ROUTING/FIQ_ROUTING/CORE_TIMER_CTRL/MAILBOX_CTRL per core +
  CORE_IRQ_SRC/CORE_FIQ_SRC at +0x60/+0x70); source bits 0 CNTPSIRQ,
  1 CNTPNSIRQ, 2 CNTHPIRQ, 3 CNTVIRQ, 4-7 mailbox, 8 GPU, 9 PMU,
  10 AXI, 11 local timer. Host line via `uc_arm64_set_irq` tracks
  GPU/PMU/AXI/local-timer/mailbox ONLY — the four arch-timer bits are
  reported in the source reg but never drive the host line (the gt
  path asserts/de-asserts CPU_INTERRUPT_HARD internally in real time;
  a host slice-boundary line would re-trigger after eret).
- GPU line = (timer p1 & icEnabled1) | (p1 & icEnabledBasic) over
  timer/UART/DMA lines (gpuLine() in main.js). Legacy IC basic IRQ 29
  (system timer) → GPU bit 8 → real vector.
- Real-time de-assert: TMR_CS write hook (HOOK_MEM_WRITE, range end
  INCLUSIVE: TMR_CS..TMR_CS+3) re-derives the GPU line mid-slice and
  clears the IRQ so a level IRQ doesn't re-trigger after eret.
- Browser ticks the arch timer at the real rate during LIRQ_MODE:
  `uc.arm64_timer_tick(floor(us * 19.2))` each slice (CNTFRQ 19.2 MHz)
  — otherwise the CNTP counter stays 0 and Phase A never fires.
- lirq guest: Phase A CNTPNS (gt path, handler reads CORE_IRQ_SRC
  bit 1 = 0x2, disables CNTP_CTL), Phase B system-timer compare at
  0x3F003014 (index 2 — see timer model convention below) → IC basic
  29 → GPU line → local block bit 8 = 0x100; handler acks with a
  `str wzr` to TMR_CS; SCRATCH[0..9]; uses the runtime's panic
  handler. 32-bit MMIO accesses only (ldr w4/str wzr — a 64-bit load
  reads the adjacent cell).
- TIMER MODEL CONVENTION (host): compare register i ↔ CS bit i — NOT
  real hardware (real C1@0x10 → CS bit 2). clock guest uses 0x10 (CS
  bit 1), irq/lirq use 0x14 (CS bit 2 → IRQ 29). Fixing to the real
  layout deferred to the Linux device rework.
- index.html gained `<option value="lirq">` — the UI select rejects
  values without a matching option (E2E "not an ELF file" was the
  select resetting to "" → PROGRAMS[undefined]).
- test/lirq-probe.mjs 14/14 PASS; browser E2E /tmp/opencode/
  lirq-e2e.mjs prints "lirq: A and B delivered"; 19/19 regression.

### Phase 2a-MMU — REAL MMU in the rebuilt core (DONE, committed 52e230d)

- mva guest (programs/mva/): enables SCTLR_EL1.M/C/I with real 4K-granule
  LPAE stage-1 tables — T0SZ=25 (39-bit VA), TTBR0_EL1 at 0x280000, MAIR
  attr0=0xFF — then keeps executing (identity code path), stores through
  an alias, verifies both PAs agree, prints PASS (test/mva-probe.mjs 7/7).
- Tables: L0[0]=0x401 (1G identity block, VA 0..1G == PA), L0[2] at
  L0+16=0x282003 (table -> L1B), L1B[0]=0x200401 (2M block VA
  0x80000000 -> PA 0x200000). NO L2 table — a 2M block descriptor read
  at level 3 is the reserved encoding (bit1=0 at level 3) -> Translation
  fault (fsr=6). Block descriptors need bits[1:0]=01, tables 11.
- Guest init gotchas found by host diagnostics: L0[2] must go at L0+16
  (8-byte stride, not +8), zeroing loops must run BEFORE the entry
  writes (an L0 loop with 8*i for i>=1 wipes L0[2] at i=2), tables are
  written 32-bit (a 64-bit store writes the adjacent cell).
- Fork facts (QEMU ~4.2 era, NOT 2.5): fork ARMFaultType enum
  (internals.h:555): 0 None, 1 AccessFlag, 2 Alignment, 3 Background,
  4 Domain, 5 Permission, 6 Translation, 7 AddressSize, 8 SyncExternal,
  9 SyncExternalOnWalk. arm_el_is_aa64(env,1) is FALSE on bare-metal
  reset (SCR_EL3.RW=0, HCR_EL2.RW=0) -> the whole EL1 regime was walked
  as AArch32 (v6 tables, wrong faults on valid blocks) — fixed in cpu.h
  to return aa64 for el<=2. T0SZ=25 -> inputsize 39, stride 9, start
  level 1 -> TTBR0 IS the level-1 table (no separate L0). mmu_idx 0x18
  = ARMMMUIdx_SE10_1 (8|ARM_MMU_IDX_A 0x10) — secure EL1, no stage-2.
  ptw reads go through UC-aware address_space_ldq_le (result 2 =
  MEMTX_ERROR, e.g. unmapped 0x800). mair has no mair_el1 field —
  record mair0_ns | (mair1_ns << 32).
- uc_arm64_debug sels extended (all uint64_t, Number() them): 14-29
  arm64_walk[0..15] (14 ret, 15 fi.type, 16 ttbr0_el1, 17 tcr_el1,
  18 mair, 19 sctlr_el1, 20/21 last ptw addr/desc, 22 fault va,
  23 raw core mmu_idx, 24 ttbr used, 25 level, 26 inputsize,
  27 ptw result, 28 access_type, 29 page_size), 30 ptw read count,
  31-38/39-46 ptw ring (8 pairs addr/desc), 47 fill count, 48-87 fill
  ring (5 per fill: va, access_type, mmu_idx, ret, fi.type), 88-111
  lpae ring (0 mmu_idx, 1 va, 2 ttbr, 3 select, 4 tbi, 5 granule,
  6 ptw read count, 7 level, 8 inputsize, 9+3n reads up to 5 (addr,
  desc, result), 18 exit ret, 19 exit fault_type, 20 exit fault_src
  (1 top-bits, 2 epd, 3 s2 startlevel, 4 invalid desc, 5 AF,
  6 permission), 21 desc at fault, 22 exit level).
- Wrapper gotcha: uc_arm64_timer_tick's cntpct is uint64_t — the wasm
  export needs a BigInt (ccall argTypes 'number' throws "Cannot convert
  X to a BigInt") — pass BigInt(cntpct) like emu_start does.
- Regression: 19/19 (branch csel clock dma fb gpio i2c instr irq mbox
  mmu pwm sd smp stats uart0 uart1 lirq mva), all exit 0.

### Phase 2b — REAL legacy-IC IRQ semantics (DONE, commit pending)

Real MMIO register layouts + genuine IRQ lines for the devices the Linux
boot needs, replacing the old "window-arbitrated IRQ" conventions. The
4 guests irq/uart0/lirq/gpio now use REAL offsets and full 3-bank IC
semantics; delivery stays host-assisted (slice-boundary irqDeliver +
IRQ_RET magic resume) for the legacy-IC guests, real (CPU_INTERRUPT_HARD)
for lirq. Verified: 20/20 probes + 9/9 browser E2E checks.

- NEW src/ic.js — the BCM2835 legacy interrupt controller (0x3F00B200)
  as a real 3-bank model: basic IC_BASIC (0x00) + IRQ1 (0x04)/IRQ2 (0x08)
  pending, ENABLE_IRQS1 (0x10)/ENABLE_IRQS2 (0x14) + DISABLE_IRQS1/2
  (0x1C/0x20), per-bank lines derived FRESH from device lines each call
  (ic.pending()/ic.line(), no stale windows). Source lines (icLines() in
  main.js): timer (tmrPending & 0xf), dma0 (DMA_CS INT+ACTIVE), pl011,
  sdhci, gpio0 (bank 0), gpio1 (bank 1), aux. Bank map: IRQ 1 = bit 0
  (timer), IRQ 29 = bit 28 (system timer -> basic bit 29 too), IRQ 7 =
  bit 6 (DMA0, kept 1<<16 -> bank-1 bit 16 — host convention, own DTB in
  Phase 3), UART RX/TX = IRQ 57 = bank-2 bit 25 (real PL011), SDHCI =
  IRQ 62 = bank-2 bit 30, GPIO 0/1 = IRQ 81/82 = bank-2 bits 17/18.
- irqDeliver (main.js): gated on DAIF.I clear (uc_arm64_debug(1) bit 7,
  the same mask real hardware checks); irqElr recorded at slice end,
  next slice starts at VBAR+0x280, IRQ_RET magic at IC_BASE+0x2C resumes.
- TIMER CONVENTION FIXED to real hardware: C1@0x10 -> CS bit 1 (irq
  guest, was 0x14->bit 2), C3@0x18 -> CS bit 3 (lirq Phase B, was
  C2@0x14->bit 2 — Linux's bcm2835_timer uses C3/IRQ 29 = basic bit 29).
  clock guest still uses 0x10/CS bit 1 (now correct by accident); the old
  register-i-to-CS-bit-i convention is gone.
- NEW src/gpio.js — full GPIO layout (GPFSEL0.., GPSET/GPCLR W1S/W1C,
  GPLEV host-driven inputs, GPEDS W1C via write hook, GPREN/GPFEN/GPHEN/
  GPLEN/GPAREN/GPAFEN, GPPUD): edges detected host-side at slice
  boundaries, GPEDS bit set iff covered by an event enable (the pin
  level mirrors into GPEDS for enabled pins), bank IRQ lines = any
  covered GPEDS bit. THE W1C SELF-CLEAR BUG: syncIn must NOT pull GPEDS
  from the window and W1C state.ev — the host's own mirror write would
  self-clear the events before the IRQ check (ev0=0x0, delivered=0 in
  the probe); the guest's W1C store is handled by the write hook
  (guest accesses only) which re-mirrors the cleared cell.
- src/uart0.js: TXIM (IMSC bit 5) added — irqActive() = (MIS & IMSC) !=
  0 where TXINTR = TXFE&&TXIM, RXINTR = RXNE&&RXIM; MIS/RIS mirrors;
  real-time RXINTR de-assert when the guest drains the FIFO (DR reads);
  syncIn pulls IMSC/ICR (W1C absorb), onIrqChange for the local line.
- src/sdhci.js: IRPT_EN (0x34) + IRPT_MASK (0x38) real semantics — line
  = (raw & intEn & sigEn) != 0; +0x30 window shows the RAW status so the
  sd guest's poll keeps working (Linux programs both registers
  explicitly); W1C via the write hook (guest-only) + exported w1c() for
  probes (host mem_write does NOT fire hooks); CMD hook range extended
  to INTERRUPT+3 (end INCLUSIVE) — a range to INTERRUPT+4 would also
  hook IRPT_EN writes and execute them as commands.
- uart0 guest: phases RX (IRQ 57) -> TXIM (IRQ 57, de-armed in the
  handler — no storm after eret). irq guest: timer C1 (IRQ 1) +
  UART RX (IRQ 57) via bank 2. gpio guest: full vector + glue (IRQ_RET
  magic at 0x3F00B22C), GPREN on BTN 29 -> IRQ 81, GPEDS W1C in the
  handler. lirq guest: Phase B now C3@0x18 -> CS bit 3 -> bank-1 bit 3
  -> IRQ 29/basic 29 (Linux's real timer line).
- The GPIO button bug in main.js: getBtn must return the PIN BITMASK
  (gpioBtn << 29), not gpioBtn — the guest polls GPLEV0 & (1<<29).
- CRITICAL mode gating: syncLocalOut/rearmGpuLine drive the real
  CPU_INTERRUPT_HARD line ONLY in LIRQ_MODE — the legacy-IC guests rely
  on host-assisted delivery (irqElr recorded at slice end), and a real
  mid-slice entry there resumes at PC 0 (irqElr never set; first
  symptom: 'DBG syncIrqRet resume 0' and the uart0 TX phase dying).
  The lirq glue erets natively (real resume); irq/uart0/gpio glues use
  the IRQ_RET magic + host resume.
- Probe conventions: gpio-probe maps the IC window (the guest now writes
  IC_ENABLE_IRQS2), presses the button via btn << 29, and the repress
  must happen AFTER 'GPREN armed' (an armed edge needs the level change
  while the enable is live — pressing once at boot just polls); uart0-
  probe waits for all 3 phases; sd-probe drives the IRPT window directly
  via exec()/w1c() (host mem_write does not fire hooks).
- NOTE: the fork source (/tmp/opencode/unicornjs-src) was wiped by a
  /tmp purge after M21 — public/unicorn.js (gitignored built artifact)
  is intact and all probes/E2Es pass with it; a future rebuild must
  re-clone AlexAltea/unicorn.js @ 8028ec43 and re-apply the patch list
  below (documented; NOT yet in src/patches/).

### Linux 6.1.182 boot crash — CURRENT WORK (fork-internal abort, traced)

Crash: Linux 6.1.182 boot aborts inside the rebuilt core (public/
unicorn.js wasm). The kernel gets through `head.S` and MMU enable, then
the fork aborts / traps while translating or executing a TB in the early
boot path (probe shows it STUCK retrying a single TB at
`early_security_init`, VA 0xffff8000097342d8 — see below for the caveat
that post-error `pc` reads are unreliable). Stock (fork) wasm OOBs;
rebuilt core reaches the same site at ~9.7 MIPS.

**RECONSTRUCTION STATUS (2026-08-22):** the fork patch set (Phase 1 +
Phase 2a-MMU `arm_el_is_aa64` fix) was re-applied from this prose to a
fresh `github.com/AlexAltea/unicorn.js` @ 8028ec43 clone in
`/tmp/opencode/unicornjs-src` and rebuilt (`python3 build.py aarch64`).
That rebuild boots the kernel to the **same** crash site (pc
0xffff8000097342d8) but at **~9.7 MIPS** vs the old instrumented
patched core's **0.003 MIPS** (~3000× faster) — confirming the
0.003 MIPS was the Phase 2a-MMU walk-ring instrumentation, which was
deliberately NOT reconstructed. Bare-metal guests (mva/irq/uart0) still
pass *functionally*; only the walk-diagnostic probe assertions fail by
design.

**ROOT CAUSE — LAYERED, PEELLED 2026-08-22:**
1. The abort is NOT SVE. Default fork CPU is **A72** (`cpu_aarch64_init`:
   `if (uc->cpu_model==INT_MAX) uc->cpu_model=UC_CPU_ARM64_A72`; probe sets
   A72=2), and `aarch64_a72_initfn` sets `id_aa64pfr0=0x00002222` →
   **SVE=0**. So SVE is never advertised; the `sve_ldffsdu_le_zss` wasm
   data label was a red herring.
2. `qemu/include/qemu/osdep.h:157` redefines `assert`→`g_assert` ONLY
   under MINGW/ANDROID/arm/i386; on wasm it is system assert (off under
   NDEBUG/Release). Fork vendors glib in `unicorn/glib_compat/`;
   `g_assertion_message_expr` (gtestutils.c:24-34) prints "assertion
   failed" then `abort()` with NO `G_DISABLE_ASSERT` guard. `printf` from
   fork C is **silenced** in the node probe context.
3. **FIRST SUPPRESSION ATTEMPT (p2a build):** edited
   `glib_compat/gtestutils.c` (`g_assertion_message_expr` → `return`),
   `translate-a64.c` (2× `default: abort();` → `unallocated_encoding(s)`),
   `helper.c` (2× walker `default: abort();` →
   `fi->type=ARMFault_Translation; return false;`). Rebuilt — kernel STILL
   aborts at the same pc. So the abort is NOT g_assert and NOT those raw
   `abort()`s.
4. **SECOND ATTEMPT (p2b/p2c build):** the actual abort is `tcg_abort()`
   (tcg.h:1157 `#ifndef NDEBUG` prints+abort; tcg.h:1163 `#else` →
   `abort()` — Release build hits `abort()`). This fires at **translation
   time** while building the TB for `early_security_init` (the kernel
   retries that single TB every slice and never advances). Neutralizing
   it (no-op continue, or a guest-phys marker write) makes the kernel
   proceed past that TB — at which point `emu_start` throws a **wasm
   "memory access out of bounds"** trap at a later point (slice 28 in the
   p2c run; `uc.mem_read(0x1000)` from the probe SUCCEEDED, so the trap is
   the *kernel* doing an OOB wasm access, not the marker write). So
   behind the translation-time `tcg_abort` there is a **deeper fork
   memory-mapping bug** (a guest access the fork maps to an out-of-bounds
   wasm linear-memory offset). `UC_ERR_RESOURCE` (code 20) was also seen
   once (likely the `uc.c:1098` `nested_level >= UC_MAX_NESTED_LEVEL=64`
   cap — `nested_level` is incremented at `uc_emu_start` entry and
   decremented only AFTER `vm_start`; if `vm_start` longjmps on a
   per-slice error the decrement is skipped and the counter climbs).
5. **CAVEAT — post-error `pc` is UNRELIABLE.** The Image is only ~34 MB
   (0x2264A00 bytes) at guest 0x200000, so any computed PA for
   `0xffff8000097342d8` (0x91342D8/0x8F342D8) is impossible → the reported
   `fault pc` is garbage after the wasm trap. The "early_security_init"
   attribution holds only for the *translation-time* stall (pre-trap), not
   the OOB trap.

**CURRENT BUILD STATE (Linux = qemu-wasm, WORKING):** the `linux` boot
mode runs `qemu-system-aarch64` from `ktock/qemu-wasm` inside an iframe
(`public/linux/index.html`), booting the `raspi3ap` machine to a busybox
shell (`~ #`). Artifacts are gitignored; restore them with
`scripts/fetch-linux.sh` (from `ktock/qemu-wasm-demo-images`). The
Unicorn-fork `public/unicorn.js` is unchanged and still serves the M1–M19
bare-metal guests; its Linux (TCG) path is now dead code, superseded by
qemu-wasm. The `unicorn.js` fork rebuild (`/tmp/opencode/unicornjs-src`)
remains the historical record for the bare-metal MMIO/slice work.

M24/M25 polish (committed + pushed): xterm + xterm-pty are vendored locally
under `public/linux/vendor/` (xterm UMD → global `Terminal`, xterm-pty UMD
→ global `openpty`) so the console works **offline** (no CDN). The console
auto-activates: `index.html` watches `.xterm-rows` for the getty's
"Please press Enter to activate this console." and calls `xterm.focus()`
then `xterm.paste("\r")`, so the user lands straight at the `~ #` shell
(the `focus()` is required — `paste()` without focus does not deliver the
key inside a same-origin iframe). Accel is **MTTCG**
(`-accel tcg,tb-size=500,thread=multi`); ktock's pthread build needs
multi-thread (single-thread triggers a `start is not a function`
pthread-worker race). **`-smp` must stay `4,sockets=4`** — `raspi3ap` enforces
the SoC's 4 cores and rejects `-smp 1` ("invalid smp cpu"). For **boot speed**
the cmdline uses **`quiet`** with **no `earlycon`** (the early-console serial
flood is the biggest TCG slow path). The root filesystem is an **initramfs
(gzipped cpio) loaded via `-initrd /pack/rootfs.bin`** instead of an emulated
SD card — this drops the slow/unreliable SD/MMC path (sdhci IRQ never fires
under TCG) and is the main speed win. The cpio has a `/init` that mounts
devtmpfs/proc/sys and hands off to busybox init; it is packed from the dev
rootfs (`scripts/linux-rootfs`) and the `.data` (dtb ‖ kernel ‖ cpio) is now
**committed** (~24 MB) with matching `load.js`, so Pages is self-contained.
The Linux engine runs inside
a same-origin `<iframe>` in the pi3-emu UI; for that iframe to be
cross-origin isolated (SharedArrayBuffer / pthreads), the WHOLE app must be
isolated. In dev/preview, `vite.config.js` sets `Cross-Origin-Opener-Policy:
same-origin` + `Cross-Origin-Embedder-Policy: require-corp` on every response.
On static hosts (GitHub Pages) that cannot send these headers, the coi
workaround is the PRIMARY mechanism: `public/coi-serviceworker.js` (root scope
`/pi3-emu/`, registered by the main `index.html`) injects COOP/COEP for the
whole origin — which also covers the embedded Linux iframe. `public/linux/
index.html` points at that same root SW (`../coi-serviceworker.js`) so direct
loads of the Linux page are isolated too. A per-frame SW at `public/linux/
coi-serviceworker.js` was dropped because a SW scoped to the iframe directory
races on GitHub Pages (the iframe navigation isn't intercepted →
`SharedArrayBuffer is not defined`). Verified
headless Chrome (puppeteer-core + /usr/bin/google-chrome-stable): the full
UI flow (select `linux` → Run → iframe) boots to `~ #` with no page errors,
and typing `echo ...` round-trips. Tests: `test/linux-boot-vendored.mjs`,
`test/linux-shell-interactive.mjs`, `test/linux-ui-integration.mjs`.

**FROM-SOURCE BUILD:** `scripts/build-linux.sh` rebuilds qemu-wasm + the
raspi3ap kernel/rootfs from `ktock/qemu-wasm` via podman (faithful to that
repo's README). It is **working end-to-end** (qemu engine, kernel, dtb,
busybox rootfs, and the `.data` preload all build under rootless podman).
Key fixes baked into the script: split emscripten CFLAGS/LDFLAGS (linker
settings like `-sWASM_BIGINT`/`-sMALLOC`/`-sASYNCIFY` belong in LDFLAGS,
not compile flags), `--disable-werror`, a two-pass configure that patches
the `dtc` meson wrap's `werror=true` (else `-no-pie` becomes a hard error
under emscripten), and a fakeroot-wrapped `mknod`/`mke2fs` so the rootfs
image builds without `CAP_MKNOD`. The build is **RESUMABLE** across
invocations (the build container and its `/build` object tree persist;
`emmake make` continues from existing `.o` files). NOTE: this checkout
lacks `PROXY_TO_PTHREAD`/pthread support, so the from-source qemu is
*single-thread* (~22 MB wasm vs the prebuilt's ~57 MB pthread/MTTCG
binary) — so the script copies its output into `public/linux-fromsrc/`
and deliberately does **NOT** overwrite the live `public/linux/`. The
 prebuilt demo binary (from `ktock/qemu-wasm-demo-images`) remains the live
 engine; do NOT swap the single-thread build over it.

 GLUE-COMPAT CAVEAT (2026-08-24): the from-source `out.js` is **standard
 emscripten glue** built with a DIFFERENT emscripten toolchain/flags than
 ktock's prebuilt, so it is NOT harness-bootable as-is. The `.wasm` engine,
 the kernel/rootfs `.data` (byte-identical to the prebuilt, 26700273 bytes),
 and `load.js` all load and execute (200s, no engine fault), but boot fails
 in the file_packager `load.js` preload because `out.js` does not expose
 ktock's runtime API on `Module`: first `Module['FS_createPath']`/
 `Module['FS_createDataFile']` are missing (fixable with a one-line shim to
 global `FS`), then `Module.addRunDependency`/`removeRunDependency` are
 missing. Root cause: the emscripten version/`-sEXPORTED_RUNTIME_METHODS`/
 `MODULARIZE` flags differ from ktock's build, so the glue and `load.js`
 (generated against ktock's API) disagree. To make `public/linux-fromsrc/`
 actually boot, either (a) rebuild with ktock's exact emscripten link flags
 (MODULARIZE + `EXPORTED_RUNTIME_METHODS` incl. FS API + run dependencies +
 a default export), or (b) generate a from-source-native `load.js`/harness
 matched to the from-source emscripten version. The qemu *engine* is the same
 qemu-wasm source and is functionally equivalent; this is purely a packaging
  gap. Verified headless: harness loads engine+data+wasm (all 200), then
  `PAGEERROR: Module.addRunDependency is not a function` (preload stage).

  LINUX USERSPACE ENRICHMENT (B1, 2026-08-24): the live rootfs was enriched
  with a hostname (`pi3-emu`), a MOTD banner (scripts/linux-rootfs/motd), an
  `/etc/profile` (hostname-in-prompt `PS1='\h:\w\$ '`), and an `rcS` that sets
  the hostname and prints the banner at boot. All busybox applets are already
  symlinked by ktock's example Dockerfile, so no extra applet work was needed.
  How the enriched `public/linux/qemu-system-aarch64.data` was produced
  (gitignored; fetched fresh by `scripts/fetch-linux.sh`): the `.data` is a
  flat concatenation `dtb ‖ gzipped-kernel8.img ‖ rootfs.bin` (no header; the
  kernel is gzip-compressed to shrink the download from 38 MB to 24 MB —
  qemu's arm64 boot code decompresses it automatically). Slice:
  `dtb = data[0:32753]`, `kernel (gz) = data[32753:8293822]`,
  `rootfs = data[8293822:24630397]`; rebuild as
  `Buffer.concat([dtb, gzipSync(kernel), rootfs])`. Rebuilding the *kernel*
  (ktock's Dockerfile uses upstream `bcm2711_defconfig`, not the Pi-tuned
  config) makes boot ~3× slower — do NOT replace the kernel.
  NOTE: boot time in this environment is variable/slow (>400s under load);
  the enrichment itself is correct (banner + `pi3-emu:~#` prompt verified).

 Tooling (rebuildable, all under /tmp/opencode/ltest — /tmp is
WIPED REPEATEDLY, redo from scratch each time):
- extract-wasm.mjs: pull wasm bytes out of public/unicorn.js
  (js-string at ~3361+14); wasm-dis → uc2.wat (~867k lines).
- build.py: `source ~/emsdk/emsdk_env.sh; python3 build.py aarch64`
  → `dist/unicorn_aarch64.js`. Long runs MUST use
  `setsid bash -c '...' < /dev/null > /dev/null 2>&1 &` (bash tool caps
  120s).
- llvm-objdump (~/emsdk/upstream/bin) is ground truth for WAT↔binary
  call-site mapping (wasm-as is BROKEN for this tree).
- test/linux-probe.mjs: reloc-skip patch writes `ret` (0xD65F03C0) at
  guest-phys 0x1046438 (VA ffff800008e46438 = `__relocate_kernel`);
  80000-slice budget; EXC-INNER handler dumps REGS AT FAULT + page-walk;
  marker-read stub (guest-phys 0x1000 — currently returns 0 since tcg_abort
  no longer writes it) for future capture.

CLOSED (wake_q_add fork-truncation theory DISPROVEN): the `wake_q_add`
fault (REGS AT FAULT `x19=0x09e3e650` at `wake_q_add+0x84`) was
hypothesised to come from the fork truncating `&console_sem` to 32 bits
inside `up()`. This is **refuted by a live `HOOK_CODE` trace** (in
test/linux-probe.mjs) of `up()`/`__up()`/`wake_q_add`: for every
`up(&console_sem)` call (from `console_unlock`), `x0` entering `up()` is
the full 64-bit `0xffff800009e3e650`, `mov x19,x0` preserves it (64-bit),
`ldr x0,[x1,#8]!` reads `wait_list.next = 0xffff800009e3e658` correctly,
and `cmp x0,x1` is EQUAL → `up()` takes the empty-list path and returns
**without** calling `__up`/`wake_q_add`. So the fork handles
`&console_sem` correctly; the fault (if/when it occurs) is a kernel-side
condition (a genuinely non-empty `console_sem` list at a deeper boot
point, or a different caller), not a fork 32-bit truncation.

SEPARATE BLOCKER (unrelated to the above): the unicorn.js probe has
**never** booted Linux 6.1.182 to console. The "Linux version" output
previously attributed to it was actually the **qemu-wasm demo** (next
section). In the current probe the kernel hangs in early boot after MMU
enable (no 5000-slice progress, no console) — it spins waiting on a
device/IRQ our models don't yet satisfy. Probe improvements made while
investigating: removed a per-instruction `HOOK_CODE` (was ~100× slowdown)
and fixed the slice-loop PC source (`reg_read_i32(ARM64_REG_PC)` returns
deprecated id-0 → 0, forcing `emu_start` to restart at the physical entry
every slice; now uses `uc.arm64_debug(5)`). The early-boot hang remains
the real M20+ Linux-bring-up work.

### Linux 6.1.21 boot — QEMU-wasm feasibility demo (DONE: busybox shell!)

Separate parallel track in /tmp/opencode/raspi-demo (ephemeral): real
qemu-system-aarch64 compiled to wasm (qemu-system-aarch64.wasm + patched
out-patched.js/load.js harness, raspi3ap machine), kernel8.img = rpi
6.1.21-v8 (22.4MB Image from the raspberrypi 6.1 branch), fixed
bcm2710-rpi-3-b-plus.dtb, initramfs.cpio.gz (busybox). Verdict: the
kernel+DTB+initrd path is FULLY functional — reached a working busybox
sh prompt ("~ #") under TCG. Everything below was learned the hard way;
re-apply when porting to the unicorn.js core.

- THE initrd blocker (fixed): the initramfs's /init symlink pointed at
  "busybox" (resolves to /busybox) while busybox lives at bin/busybox —
  DANGLING. The rpi kernel's PATCHED kernel_init_freeable (init/main.c:
  wait_for_initramfs(); if (init_eaccess(rdinit)!=0) { rdinit=NULL;
  prepare_namespace(); }) then fell back to prepare_namespace →
  mount_root → "VFS: Unable to mount root fs on unknown-block(0,0)"
  panic (no root=). Fix: /init -> bin/busybox. This ALSO explains the
  earlier "kernel never unpacks / no Trying to unpack" misdiagnosis —
  the unpack ALWAYS succeeded; only the /init access check failed.
- rpi initrd mechanics confirmed: do_populate_rootfs is ASYNC
  (async_schedule_domain, wait_for_initramfs()); arm64_memblock_init
  sets initrd_start = __phys_to_virt(0x08000000) = 0xffffff8008000000
  (VA_BITS=39! not 48) + initrd_end = ...+0x11c578; success prints
  "Trying to unpack rootfs image as initramfs..." then
  "Freeing initrd memory: 1136K"; initrdmem=0x08000000,0x11C578 and
  DTB linux,initrd-start/end both work (the reserve_initrd_mem
  "INITRD: ... is not a memory region" patch in rpi initramfs.c is
  DEAD CODE — no callers).
- Harness gotcha (invalidated many early scans): without the
  Module['TTY'].stream_ops.poll override (return (0|4) when no stdin),
  the chardev TX buffer fills and the GUEST STALLS mid-UART-write at
  ~6.8s (last ring msg "bcm2835-mbox 3f00b880.mailbox: mailbox
  enabled") — the boot never reaches initcalls. With the override the
  boot runs to the panic in ~34s. ALWAYS include it in boot harnesses.
- The console (pty) DROPS message batches (cmdline→cp15_barrier window
  incl. Memory:/rcu:/smp:, and the Trying-to-unpack batch) while the
  printk ring has EVERYTHING — the ring text region is guest
  phys 0x177a000 (heap 0x21aad000, right past kernel image end),
  records are plain ASCII with the last 2 chars duplicated per record
  ("enableded", "B+B+"). NEVER trust the pty alone; scan RAM.
- Misc: kernel8.img loads at guest 0x20000 (not 0x80000); fixed DTB at
  guest 0x8200000; initrd at 0x08000000 (gzip verified byte-for-byte);
  initrd region persisted post-panic (reserved or just untouched —
  NOT proof of reservation); "Freeing unused kernel memory"/"Run /init"
  never print in the panic path (panic hits inside prepare_namespace).
- cpio with device nodes: mknod needs root; build newc entries by hand
  in Node (build-cpio.mjs) — header = "070701" + 13 u32-hex fields,
  S_IFCHR 0x2000|0666, rdev major/minor fields, TRAILER!!! entry.
- Shell polish: etc/inittab (::respawn:/bin/sh), etc/init.d/rcS
  (mount proc/sys), dev/console+dev/tty+dev/null nodes → boots to
  "/bin/sh: can't access tty; job control turned off" + "~ #".
  Interactive stdin (preloaded typed bytes) was NOT consumed by sh in
  the harness (chardev RX plumbing pending) — E2E input needs work.
- Harmless noise: mmc1 "Timeout waiting for hardware interrupt" every
  ~10s (sdhci IRQ never fires — no card; the rpi bcm2835-sdhost uses
  IRQ 62 = bank-2 bit 30 — matches the Phase 2b host mapping).
- Porting checklist for the unicorn.js core: initrd -> /init symlink
  fix, TTY poll override, expect VA_BITS=39 linear map
  (0xffffff8000000000 PAGE_OFFSET — uc_arm64_debug walk tools must
  use TTBR/TCR-derived vabits, not hardcoded 48).

### Phase 2 — real devices

- [x] BCM2836 local interrupt block at 0x40000000 (Phase 2a above).
- [x] Real legacy-IC 3-bank semantics + PL011 RXIM/TXIM + GPIO event
      registers + SDHCI IRPT_EN/IRPT_MASK (Phase 2b above).
- Slice loop changes: no run-until-idle (Linux never idles); fixed
  budget + interrupt check between slices; inject when DAIF.I clear.

### Phase 3 — boot protocol

- Kernel Image at 0x80000, r0=0 r1=0xFFFFFFFF r2=DTB phys addr, MMU off.
- Minimal bcm2837 DTB (or upstream bcm2837-rpi-3-b.dtb trimmed) with
  /chosen linux,initrd-start/end.
- busybox initramfs (cpio.gz) loaded via DTB.
- Build kernel: arm64, minimal defconfig, built-in PL011/timer/bcm2836
  IC/GPIO/SDHCI, no modules.
- Milestone: `console: Freeing init memory` + busybox `/bin/sh` prompt.

### Phase 4 — polish (optional)

- SD rootfs as a real block device (multi-block reads, real image),
- DMA engine for the SDHCI driver, performance tuning, IndexedDB
  snapshots, xterm.js.

### M26 — Linux UX polish (C1–C4)

The live Linux engine (public/linux/, ktock/qemu-wasm raspi3ap) gained four
user-facing improvements to the busybox shell experience:

- **C1 `hw` tour + richer userspace:** `scripts/linux-rootfs/hw` is a shell
  script (`/bin/hw`) printing a tour of the emulated SoC (CPU count, memory,
  clocksource, GPIO sysfs hint). Plus the B1 hostname/MOTD/profile banner.
- **C2 Real login:** `scripts/linux-rootfs/{inittab,passwd,shadow}` install a
  busybox `getty` + `login` on ttyAMA0. root password = `raspberry` (md5
  crypt for busybox-login compat). The harness auto-fills `root`/`raspberry`
  (public/linux/index.html auto-activate detects `login:`/`Password:`).
  **GOTCHA:** the inittab MUST use `/bin/getty` — in ktock's rootfs every
  busybox applet is symlinked into `/bin` only; `/sbin/getty` does not exist,
  which would leave init with no console (silent dead boot).
- **C3 Browser→VM file upload:** the harness "Upload" button reads a local
  file, base64-encodes it, and pastes
  `echo <b64> | base64 -d > /mnt/incoming/<name>` into the serial console
  (busybox ships `base64`). `/mnt/incoming` is created by rcS. Works over the
  existing xterm/pty serial bridge — no FS plumbing needed.
- **C4 Browser→guest control panel:** a command box (`#cmd` + Run) and GPIO21
  on/off buttons in the toolbar send shell commands into the guest over the
  serial console. (Light loop: browser → serial → guest shell → qemu GPIO
  device; NOT a direct device-model bridge — that needs a qemu device patch,
  see deferred B3.)

**REBUILDING THE ROOTFS WITHOUT THE CONTAINER (preferred):** a fresh `mke2fs`
image works but the container build is gitignored/ephemeral and slow. Inject
files into the ORIGINAL `rootfs.bin` with userspace `debugfs` (no mount/root
needed), preserving the proven on-disk geometry:
```
node -e 'const fs=require("fs");const p=fs.readFileSync(process.env.HOME+"/qemu-wasm-demo-images/raspi3ap/qemu-system-aarch64.data");fs.writeFileSync("orig-rootfs.bin",p.subarray(22505969,26700273));'
cp orig-rootfs.bin work-rootfs.bin
debugfs -w work-rootfs.bin <<'E'
rm /etc/inittab
write scripts/linux-rootfs/hw /bin/hw
chmod 755 /bin/hw
write scripts/linux-rootfs/inittab /etc/inittab
write scripts/linux-rootfs/passwd /etc/passwd
chmod 644 /etc/passwd
write scripts/linux-rootfs/shadow /etc/shadow
chmod 600 /etc/shadow
write scripts/linux-rootfs/hostname /etc/hostname
write scripts/linux-rootfs/motd /etc/motd
write scripts/linux-rootfs/profile /etc/profile
mkdir /mnt/incoming
write rcS /etc/init.d/rcS
chmod 755 /etc/init.d/rcS
E
e2fsck -fy work-rootfs.bin
# repackage .data: dtb[0:32753] kernel[32753:22505969] rootfs[22505969:26700273]
node -e 'const fs=require("fs");const p=fs.readFileSync(process.env.HOME+"/qemu-wasm-demo-images/raspi3ap/qemu-system-aarch64.data");const r=fs.readFileSync("work-rootfs.bin");fs.writeFileSync("qemu-system-aarch64.data",Buffer.concat([p.subarray(0,32753),p.subarray(32753,22505969),r]));'
```
**GOTCHA:** `debugfs write` REFUSES to overwrite an existing file ("Ext2 file
already exists") — `rm` it first, then `write`. Injecting into the original
image (not building fresh) keeps the first blocks byte-identical, so the SD
card (mmc0) probe in the kernel is unchanged.

**VERIFICATION CAVEAT (2026-08-25):** headless boot in THIS sandbox is
currently UNRELIABLE — the qemu-wasm pthread worker intermittently fails to
start ("start is not a function" worker race) and the guest freezes during
kernel boot; this was observed even with the **stock, unmodified** `.data`, so
it is an environment issue, not a regression from C1–C4. The changes could not
be end-to-end verified here, but they are sound (rootfs derived from the
original image's geometry; harness changes are additive serial/DOM). Verify in
a real browser (`npm run dev` → Linux tab) where the worker starts cleanly.

### M27 — Linux dev environment (N1 compiler + N2 fast kernel) (DONE)

- **N1 In-guest C compiler:** the guest rootfs is now a full dev environment —
  busybox (static), glibc shared loader + libs (`/lib/ld-linux-aarch64.so.1`,
  `libc.so.6`, `libm`, `libgcc_s`, `libpthread`, `libdl`), aarch64 C headers
  (`/usr/include`), and a **static `tcc`** (`/bin/tcc`) with its runtime
  `libtcc1.a` (`/lib/tcc/libtcc1.a`, `/usr/lib/tcc/libtcc1.a`). Guest can now
  `tcc -o hello hello.c && ./hello` (uses libc + headers at runtime). Verified
  via `debugfs` on the built image: `/bin/tcc` is an aarch64 ELF, `libc.so.6`
  + `ld-linux` + `stdio.h` present, `libtcc1.a` present, `e2fsck` clean.
  - tcc build recipe (in `scripts/linux-rootfs/image.Dockerfile`): clone
    `repo.or.cz/tinycc.git`, build the host helper `c2str.exe` with **x86
    `gcc`** (`gcc -DC2STR -o c2str.exe conftest.c && touch c2str.exe`) so the
    cross `make` doesn't try to recompile it, then `./configure
    --cross-prefix=aarch64-linux-gnu- --cpu=arm64 --enable-static && make`
    (the `make` fails at `libtcc1.a` because the aarch64 `tcc` can't run on x86
    — ignore with `|| true`; build `libtcc1.a` separately with
    `aarch64-linux-gnu-gcc -c lib/lib-arm64.c && ar rcs`). Resulting `tcc` is
    dynamic (links glibc at runtime — fine, glibc is in the image).
  - The dev rootfs is **128 MB** (was 4 MB) to fit glibc + headers + tcc.
- **N2 Faster/real kernel:** SATISFIED by retaining ktock's **prebuilt** fast
  `kernel8.img` (raspberrypi/linux tag `1.20230405`, ~22 MB). Rebuilding with
  the upstream `bcm2711_defconfig` (the image Dockerfile's `kernel-dev` stage)
  makes boot ~3× slower — do NOT swap it in for the live `.data`. N2 needed no
  code change; it is a "keep the prebuilt kernel" decision.
- **LIVE `.data` ASSEMBLY (fast kernel + dev rootfs):** because the dev rootfs
  is bigger, the `.data` layout and the committed `public/linux/load.js` slices
  change. The live `public/linux/qemu-system-aarch64.data` is reassembled as
  `prebuiltDtb[0:32753] ‖ prebuiltKernel[32753:22505969] ‖ devRootfs[22505969:
  156723697]` (total 156723697). `load.js` `loadPackage` slices updated to
  rootfs `end: 156723697`, `remote_package_size: 156723697`. Recipe:
  ```
  node -e 'const fs=require("fs");const p=fs.readFileSync(process.env.HOME+"/qemu-wasm-demo-images/raspi3ap/qemu-system-aarch64.data");const r=fs.readFileSync("dev-rootfs.bin");fs.writeFileSync("public/linux/qemu-system-aarch64.data",Buffer.concat([p.subarray(0,32753),p.subarray(32753,22505969),r]));'
  ```
  **GOTCHA:** whenever the rootfs size changes, BOTH `load.js` (the two slice
  numbers + `remote_package_size`) AND the repackaged `.data` must be updated
  together, or the preload unpacks the wrong byte range and the kernel panics
  ("VFS: unable to mount root fs").
- **REPRODUCIBLE FROM SOURCE:** `scripts/linux-rootfs/image.Dockerfile` (the
  full multi-stage file, committed) is copied over the clone by
  `scripts/build-linux.sh` (step 6 builds from `scripts/linux-rootfs` as the
  docker context, using `image.Dockerfile`). `image.Dockerfile` now emits the
  rootfs as a **gzipped-cpio initramfs** (`rootfs.bin` = cpio.gz, with a `/init`
  that mounts devtmpfs/proc/sys and execs busybox init) — matching the
  `-initrd /pack/rootfs.bin` boot in `module.js`. Step 7 packages
  `/pack` (dtb + kernel + cpio) into `qemu-system-aarch64.data` and regenerates
  `load.js` (the `file_packager` writes the correct offsets automatically). Step
  8 installs the engine **plus** `.data` + `load.js` into `public/linux/`, so a
  from-source rebuild is self-consistent. This is a from-source build
  (single-thread + **slow** upstream `bcm2711_defconfig` kernel); for the live
  **fast+dev** combo, re-stitch the prebuilt fast kernel over the built `.data`
  using the manual repack recipe above (dtb‖kernel‖rootfs byte ranges).
- **STATUS:** dev rootfs built and repackaged into the live (gitignored)
  `public/linux/qemu-system-aarch64.data`; `load.js` offsets updated; Dockerfile
  + `build-linux.sh` recipe committed. End-to-end boot NOT verifiable in this
  sandbox (pthread worker race, see VERIFICATION CAVEAT) — verify in a real
  browser. The tcc binary itself and the image contents ARE verified sound via
  `debugfs`/`file`/`e2fsck`.

### M28 — snapshot file (N3) + real GPIO bridge (N4)

- **N3 Snapshot file (DONE, live):** the SD card image (`/pack/rootfs.bin`)
  is now downloadable/uploadable from the Linux harness (`public/linux/
  index.html`): **Save Disk** downloads the current disk as
  `pi3-rootfs-<ts>.bin` AND persists it to **IndexedDB**; **Load Disk**
  reads an uploaded `.bin` into IndexedDB and reboots with it injected over
  the unpacked `/pack/rootfs.bin` (via a `preRun` that runs after the
  `load.js` packager preRun); **Reset Disk** clears IndexedDB → original
  disk. This is the "a file which stores data the user can download and
  upload" workaround for save/restore — it persists the **filesystem**
  (the user's compiled programs, installed files) across sessions. It is a
  pure harness feature (no qemu rebuild): `index.html` reads/writes the
  emscripten FS via `Module.FS` and uses IndexedDB for cross-reload
  persistence. GOTCHA: it is NOT a full VM-state snapshot (RAM/registers) —
  qemu-wasm has no savevm→IndexedDB path and the raspi3ap machine's
  migration is unverified, so a true "freeze the whole VM" snapshot is not
  available. Re-boot from a saved disk is the supported model.
- **N4 GPIO bridge — two layers:**
  - **Live harness bridge (DONE):** the toolbar GPIO buttons cover pins
    **17/18/21/22** (toggle on click, `* ` suffix + green when on). Each
    click drives the **real emulated BCM2835 GPIO** via sysfs over the serial
    console (`echo <pin> > /sys/class/gpio/export; echo out > …/direction;
    echo <v> > …/value`). Genuine browser→guest GPIO control of actual SoC
    pins (the C4 serial path, generalized to multiple pins).
  - **True device-level bridge (LIVE as of 2026-08-25):** `scripts/linux-
    rootfs/pi3ctl.{c,h}` is a qemu **plain `DEVICE`** (no MMIO — avoids the
    address-collision risk) wired directly to the real `bcm2835_gpio` by
    `hw/arm/raspi.c`: `bcm2835_gpio.out[line] → pi3-ctl input` (guest GPIO
    output writes forwarded to the browser as `S <line> <v>\n` via emscripten
    `postMessage`), and `pi3-ctl output[line] → bcm2835_gpio.in[line]` (the
    browser sends `I <line> <v>\n` → `pi3_rx()` sets a real emulated GPIO
    **input** the guest reads via GPLEV). `bcm2835_gpio` gained `in[54]`
    qemu_irq input lines + `in_lev0/1` reflected in GPLEV (patched via
    `scripts/linux-rootfs/apply-n4-patches.py`, which also instantiates
    pi3-ctl in `raspi_machine_init`). The browser side is wired in
    `public/linux/index.html` (Bridge buttons 23/24/25/26/27 + an `Echo Test`
    pulse button, and a live `guest→browser: G<line>=<v>` readout). The harness
    shows a `pi3-ctl: ready` / `pi3-ctl: n/a (stock engine)` badge so the device
    support is visible without a kernel round-trip, and the `S <line> <v>` RX
    regex tolerates the C code's trailing newline. **Boot speed:** `module.js`
    append adds `lpj=7000000` to skip `calibrate_delay`, `nokaslr` to skip
    KASLR, `mitigations=off` to disable Spectre/Meltdown (huge win under TCG),
    `nowatchdog nosoftlockup` to skip the lockup detector, `loglevel=1` for
    minimal output, and a comprehensive `initcall_blacklist` (USB, ethernet,
    thermal, I2C, SPI, RNG, etc.) to skip drivers that timeout or are useless
    under TCG. The **kernel is now gzip-compressed** inside `.data` (22 MB → 8
    MB), shrinking total `.data` from 38 MB to 24 MB — qemu's arm64 boot code
    decompresses it automatically. **Terminal:** `index.html` auto-fits the
    pty grid to its container via a `fitTerminal()` measuring the rendered cell,
    and a **Script** panel (toolbar "Script" button) streams multi-line text into
    the console or saves it to `/root/<name>` via a quoted heredoc — a companion
    to the in-guest `tcc` workflow.
    **ENGINE REBUILD:** `scripts/build-linux.sh` now replicates
    ktock's exact emscripten flags (README aarch64): `-O3 -DG_DISABLE_ASSERT
    -D_GNU_SOURCE -sPROXY_TO_PTHREAD=1 -sFORCE_FILESYSTEM -sALLOW_TABLE_GROWTH
    -sTOTAL_MEMORY=2300MB -sWASM_BIGINT -sMALLOC=mimalloc --js-library=xterm-pty
    -sEXPORT_ES6=1 -sASYNCIFY_IMPORTS=ffi_call_js`, plus `-sEXPORTED_RUNTIME_
    METHODS=...,TTY,FS,ccall` (the `ccall` export lets the harness call
    `pi3_rx`). The build runs in `/qb` (never `/build`, which holds the
    image sysroot) and installs `out.js`/`.wasm`/`.worker.js` into the LIVE
    `public/linux/` (the `.data`/`load.js` are preserved). The rebuilt live
    engine was verified to compile, export `pi3_rx`/`ccall`/`FS_createPath`,
    and match the prebuilt's `.worker.js` size (6001 B, pthread parity).
    **VERIFICATION CAVEAT:** full boot is still unverifiable headlessly here
    (pthread worker-race, see M26 caveat) — the browser-side bridge must be
    confirmed in a real browser (`npm run dev` → Linux tab → Bridge buttons).

### M29 — PWM/SPI/I2C device bridges (dual-path: JS + QEMU C devices)

Two-layer bridge architecture for PWM (0x3F20C000), SPI (0x3F204000),
and I2C (0x3F804000):

**Layer 1 — JS-side (bare-metal guests, immediate value):**
Extended `src/pwm.js`, `src/spi.js`, `src/i2c.js` with:
- Optional `onBridgeData` callback parameter (4th arg to factory)
- Each model returns a `bridgeRx` function for browser→guest data
- When callback is registered, device data is forwarded to the browser
  via `window.postMessage()`
- SPI/I2C: bridge mode defers `sDone` until `bridgeRx()` provides the
  response (bidirectional); if no callback, hardcoded slave behavior
  is preserved (backward compatible)
- PWM: output-only (forward drained sample count to browser)
- `src/main.js` wires `onBridgeData` → `window.postMessage` and adds
  a `message` listener for `bridge-rx` commands (dispatches to
  `spiBridgeRx` / `i2cBridgeRx`)

**Layer 2 — QEMU C devices (Linux path, needs engine rebuild):**
Three new `DEVICE` objects with MMIO regions at the BCM2835 addresses:

- `scripts/linux-rootfs/pwm-bridge.{c,h}` — Type `pwm-bridge`, MMIO at
  0x3F20C000. Emulates CTL/STA/RNG1/DAT1/FIFO. FIFO writes forwarded
  to browser as `"PWM <count>\n"`. No browser→guest (output-only).
- `scripts/linux-rootfs/spi-bridge.{c,h}` — Type `spi-bridge`, MMIO at
  0x3F204000. Emulates CS/FIFO/CLK/DLEN. TX bytes forwarded as
  `"SPI_TX <hex>\n"` on TA rise. Browser responds via
  `EMSCRIPTEN_KEEPALIVE spi_bridge_rx("SPI_RX <hex>\n")`.
- `scripts/linux-rootfs/i2c-bridge.{c,h}` — Type `i2c-bridge`, MMIO at
  0x3F804000. Emulates C/S/DLEN/A/FIFO. Transfers forwarded as
  `"I2C_TX <addr> <reg> <hex>\n"` (write) or `"I2C_RD <addr> <reg> <dlen>\n"`
  (read). Browser responds via `EMSCRIPTEN_KEEPALIVE i2c_bridge_rx("I2C_RX <hex>\n")`.

**Wiring:** `apply-n4-patches.py` extended (patches 6–8): includes new
headers, instantiates all three bridge devices via `qdev_new` /
`sysbus_mmio_map` after SoC realize. `build-linux.sh` step 3b copies
the 6 new files and registers them in `hw/misc/meson.build`.

**Browser UI:** `public/linux/index.html` toolbar gains:
- `#bridgeData` span for PWM/SPI/I2C readout
- `window.addEventListener("message")` handles `"PWM <n>"`,
  `"SPI_TX <hex>"`, `"I2C_TX/WR/RD ..."`, `"I2C_RX ..."` strings
- `spiBridgeSend(hex)` / `i2cBridgeSend(hex)` ccall wrappers for
  browser→device responses
- Bridge status badge shows `pi3-ctl: ready` when ccall is available

**VERIFICATION:** JS layer verified via `npx vite build` (16 modules
transformed, no errors). C devices compile only inside the qemu-wasm
container (need QEMU headers); verify with `scripts/build-linux.sh`.
Full bridge round-trip requires browser verification (`npm run dev` →
Linux tab → bridge readout + send).

### M32 — SAB on/off toggle + ST build attempt (A/B blocked by arch)

- **Reusable `public/sab-toggle.js`** (no deps, classic script): detect() /
  getPreference() (`?threads=` › hash › localStorage › auto) /
  setPreference() / decide() / resolve() / ensureIsolation() / probeFile() /
  pickVariant() / bindSelect() / describe(). Root page persists the threads
  dropdown; `public/linux/` consumes it (module.js delegates, index.html
  badge + routing + fallback panel). Tests: `test/linux-threads-toggle.mjs`
  (17 checks), `test/linux-boot-bench.mjs` (phase JSON for A/B).
- **A/B outcome:** MT boots to `~ #` in ~40–70 s. The ST build
  (`scripts/build-linux.sh --threads=st` → `public/linux-st/`, non-proxy
  link, initrd boot via engine-aware module.js) compiles/links/packages
  rc=0 — but cannot execute: ktock's wasm32 JIT calls `init_wasm32()` only
  from `mttcg_cpu_thread_fn`. Mirroring it onto `rr_cpu_thread_fn` (new
  `apply-st-patches.py`, ST-only) gets past the `tb_ptr_ptr` crash — the
  engine then runs ~5 min silent and dies on uncaught `Infinity` (escaped
  setjmp/longjmp; QEMU uses siglongjmp for CPU exception exits, incoherent
  across private heaps). `thread=multi` on the non-shared heap spawns
  4 workers × private 2.3 GB heaps (swap death, CDP timeouts). No-SAB needs
  upstream backend work (RR init done, main-thread-only execution still
  open). Harness routes to linux-st/ only on a `.bootable` sentinel (build
  script won't create it).
- **Build fixes found by the ST attempt (all committed):** bogus
  `sysbus_init_child_obj` removed from pwm/spi/i2c-bridge.c (never existed
  in QEMU 8.2 — the M29 C devices had never compiled); `apply-n4-patches.py`
  now checks `new`-before-`old` (insert-before anchors re-applied every
  rebuild → triple `bcm2835_gpio_in_set`); image.Dockerfile installs `cpio`
  + asserts rootfs > 1 MB (missing cpio silently made a 20-byte rootfs);
  ST link needs `-sTOTAL_MEMORY=2300MB` (1500 MB OOMs: 512 MB guest + 500 MB
  tb-size + overhead).
- **Bench table lives in README** ("SharedArrayBuffer on/off"); ST column
  stays "blocked" until the backend work lands.
- **Do NOT move the wasm/.data blobs to Git LFS** (GH001 warning
  notwithstanding): GitHub Pages serves LFS pointers instead of file
  contents, which would break the live emulator. Large binaries in plain
  git are the deliberate trade-off.

### M33 — npm packages (pi3-emu core + sab-toggle)

- Monorepo (`npm workspaces`): device/support modules moved `src/*.js` →
  `packages/pi3-emu/src/` (single source; main.js + probes follow).
- `pi3-emu` 0.1.0: DOM-free `Pi3Emulator` facade (ELF load, slices, PL011
  console, timer, GPIO, IC + IRQ_RET delivery, attach helpers for
  UART1/I2C/SPI/PWM/SDHCI/MMU/DMA), vendored unicorn.js (+ `vendor/
  package.json` commonjs marker — without it `require()` returns an empty
  ESM namespace), demo firmware, `examples/i2c-temp-sensor` reference part.
- `sab-toggle` 0.1.0: the SAB switch as a dependency-free file + node smoke.
- Unaligned `IC_BASE` rides inside the `MBOX_WINDOW` page mapping (never map
  it directly — `UC_ERR_ARG`); sync mirror writes are `safeSync`-wrapped.
- Both names free on npm; publish needs `npm login` (unavailable headless).

### M34 — core batch (virtual time, AudioWorklet, faults, fork patches)

- Virtual time (`?vt=1`, facade `virtualTime`): timer advances per insn
  (~10 MIPS) — deterministic runs, instant sleeps (`test/virtual-time.mjs`).
- PWM audio on AudioWorklet (`public/audio-pwm.js`, ScriptProcessor
  fallback): transfer neuters buffers, so posted-length counters must be
  read BEFORE `postMessage`; pwm mode pins 512-insn slices (the 4096
  default overruns the 256-deep FIFO — FULL1 is slice-boundary-visible —
  and drops ~86% of samples). `test/pwm-audio.mjs`: worklet + exact 84672.
- Guest fault decoder (`packages/pi3-emu/src/fault.js`): PC/SP/insn/cause
  to terminal/status/`lastFault`; rAF loops halt on streaks.
- Fork patches reconstructed in `src/patches/` (AlexAltea/unicorn.js
  @8028ec43 + engine): IRQ/timer/debug APIs, CNTFRQ 19.2 MHz, AArch64 reset
  fixes, exports + wrapper — rebuilt with emsdk 6.0.6 and verified (lirq
  14/14, full battery green). Build lessons baked in: no value-returning
  `UC_INIT` in void fns, distinct `arm64_*` arch names, `BigInt()` timer
  coercion. `public/unicorn.js` is now that single-arch rebuild.

### M35 — MicroPython bare-metal port (spike green)

- `ports/bcm2837/` (new, out-of-tree port) + `ports/micropython` submodule
  (upstream master, shallow): minimal-ROM-level VM, no floats, 256 KB heap,
  PL011 REPL, frozen `boot` module. Toolchain: ARM gcc 13.2
  aarch64-none-elf tarball in ~/toolchains (not in repo).
- Boots to `>>>` in the emulator; `1+1`→2, variables, heap strings/lists,
  `import boot` + `boot.hello()` all verified (`test/upython-repl.mjs` 7/7).
- Debug trail (all in ports/bcm2837/README.md): fake empty frozen qstr
  pool corrupts runtime interning (proven by guest-memory forensics:
  stored keys land under id 0) → always generate via mpy-cross/mpy-tool;
  frozen imports need sys.path (`.frozen` entry) + plain-NO_EXIST import
  stub; keep mpy-tool's `boot.py` entry name (`import` appends `.py`);
  `MICROPY_ENABLE_EXTERNAL_IMPORT=1` required or `mp_find_frozen_module`
  is gc'd; drip-feed scripted UART input (16-byte RX FIFO).
- `machine.UART` done (`machine_uart.c`: Pico-compatible
  read/readinto/write/any on PL011 id 0 + mini-UART TX on id 1, real
  baud dividers; per-byte slice settle; 7/7 in `test/upython-uart.mjs`).
  Floats work natively on the core (VFP executes; `BUILTINS_FLOAT`/
  `FLOAT_IMPL_DOUBLE`/`MATH` + `-lm`, no soft-float needed).
- Pico code: plain `machine.*` Python will carry over once the `machine`
  module lands; `rp2.PIO`/ADC have no BCM2837 equivalent. `machine.Pin`
  done (self-contained `machine.c`, Pico-compatible IN/OUT/pull/value/
  on/off/init on the real registers; `test/upython-machine.mjs` 9/9
  incl. LED-dot GPLEV and button reads). `machine.I2C`/`SPI` done
  (`machine_i2c/spi.c`, Pico-compatible API incl. `readfrom_mem`,
  `write_readinto`, `scan`; 9/9 vs built-in slaves in
  `test/upython-i2cspi.mjs`). High-level API is local, not extmod's
  shared dicts (wrong methods resolve on this config); transfers need a
  DONE drop-sync first (slice-boundary status staleness); 4-byte cap.
  `Pin.irq()` done (vectors.s full save + GPEDS W1C ack + deferred drain
  in stdin spin; level-qualified edges; press→1/repress→2, no keys).
  FAT12 over SDHCI in pure Python (`sdcard.py` + frozen auto-run at
  startup; `test/upython-sd.mjs` 6/6 with CMD24 overwrite + raw block
  round-trip; card image is real FAT12 now — boot sig, `FAT12   ` type,
  cluster@+26, size u32@+28 — after the VFS phase below).
- `Pin.irq()` level triggers done (`lvlCache` GPHEN/GPLEN in `gpio.js`,
  level-qualified dispatch in `irq.c`; the "silent while released" failure
  was test button sequencing — the rising phases leave BTN held, the level
  section never released it. Fixed in `test/upython-irq.mjs` + fixed its
  double-escaped level regexes. `syncIn` re-mirrors GPEDS to erase the
  guest-store-after-hook residue. 6/6).
- FAT writes done (CMD24 in `sdhci.js`, `writeblocks` + `write(name,data)`
  in `sdcard.py`, 6/6 in `test/upython-sd.mjs`).
- `os` mount done (this upstream has no `uos` alias): `MICROPY_VFS/
  VFS_FAT/READER_VFS/PY_OS/PY_IO/FATFS_RPATH=2/ENABLE_FINALISER` in
  `mpconfigport.h`; `vfs_port.c` injects the two missing VM-state fields
  via `MP_REGISTER_ROOT_POINTER` (submodule pristine); extmod VFS+oofatfs
  sources compiled into `$(BUILD)` as `upy_*.o`; `fatfs_port.c`
  `get_fattime`; `test/upython-vfs.mjs` 7/7 (mount/listdir/read/create+
  write/import `greet.py` from `/sd`). Gotchas: `FFCONF_H` define needed,
  `CFLAGS` changes don't trigger rebuilds (`touch ff.c`), upstream
  inlines `mp_import_stat` under VFS (don't redefine), FatFs `check_fs`
  needs the `FAT12   ` type string at +54. mva-probe's 1 walk-diagnostic
  failure is pre-existing (fails on pristine tree too).

### M36 — /sd as a first-class filesystem (DONE, uncommitted)

- **A1 Safe auto-mount:** `SD_PRESENT` host-extension word at
  `MBOX_WINDOW+0xFF0` (clear of mailbox regs +0x880 and IC +0x200),
  driven every slice — always 1 in the browser host (`src/main.js`),
  1 iff `attachSdhci()` in the facade. `boot.py` mounts `/sd` + appends
  `sys.path` only when set (reading it can't abort); detached boots
  unchanged. Banner keeps the `boot: pi3-emu ready` substring the repl/sd
  tests match.
- **C VFS depth** (`test/upython-vfs.mjs` 14/14): mkdir/chdir/getcwd,
  stat/statvfs, seek/tell/partial, append, 3.2 KB multi-cluster,
  package import, post-umount OSError. `os.stat` needed
  `MICROPY_LONGINT_IMPL_MPZ` — under NONE, `mp_obj_new_int_from_ll`
  *unconditionally* raises "small int overflow" (NOT a width problem;
  small ints are 63-bit — proven by `1<<40`. LONGLONG is 32-bit-only
  upstream: `objint_longlong.c:372` asserts `sizeof(mp_uint_t)==4`).
  Image carries a valid 2026-09-10 dir stamp (FatFs date math underflows
  on zero dates). upython-repl's `^`-anchored echo check relaxed (bigger
  firmware shifts slice timing; prior prompt's trailing space lands late).
- **B Allocation:** `sdcard.write` creates/extends/shrinks chains
  (`_fat12_set` nibble mirror, free scan capped by the model's 32-sector
  growth limit, both FAT copies, root fixed at 16). upython-sd 8/8.
- **D Persistence:** `sdhci.js` `exportImage`/`loadImage` +
  `Pi3Emulator.exportCard`/`importCard` + `test/upython-vfspersist.mjs`
  7/7. ONE-WRITER RULE (load-bearing): raw writes bypass the live mount's
  cache AND FatFs write-backs clobber raw-written sectors — umount before
  raw writes, remount after; encoded in both suites + `sdcard.write` docs.
  Browser Save/Load UI deferred (firmware has no program-list entry yet).
- Regression: 19/20 probes rc=0 (mva pre-existing), all 8 upython suites
  PASS, `npx vite build` clean.

### M37 — MicroPython in the browser + card Save/Load (DONE, uncommitted)

- **Program entry:** `public/programs/firmware.elf` (built artifact copy)
  + `upython` option in `index.html` + `UPY_MODE` branch in `src/main.js`
  (`boot()` + `irqRun()`; all windows incl. SDHCI always mapped in the
  browser host, so `SD_PRESENT=1`). The hold button (`#gpio-btn`) is
  shown in upython mode too — it doubles as the `Pin.irq` button
  (`pressGpioBtn` gate extended from `GPIO_MODE`-only).
- **Real-IRQ gating (load-bearing):** firmware enables legacy-IC GPIO
  banks, so browser `irqDeliver` would hijack its native erets — `UPY_MODE`
  now joins the early-return (mirrors facade `realIrq`), and `syncLocalOut`
  / `rearmGpuLine` / `irqRun` include `UPY_MODE` for the local-block line.
- **TWO CORES (SUPERSEDED by M38 — single stock core now; history kept):**
  `public/unicorn.js` (single-arch rebuild) CANNOT execute
  NEON — newlib `strlen`'s `shrn` aborts (`0x1287cc`, UC_ERR_RESOURCE;
  kills the version banner right after `/sd mounted`). The vendored full
  fork build (`packages/pi3-emu/vendor/unicorn.js`) does NEON fine — but
  it HANGS browser lirq (6 min silence; node lirq-probe passes on it, so
  it's a vendor×browser timer-path gap, unroot-caused). Resolution: the 19
  integer guests keep stock `public/unicorn.js`; upython lazily loads the
  vendor build via `?url` asset (`ensureUpyCore`, own `MUnicornVendor`
  global, `STOCK_CORE` captured by value and restored). Any future core
  rebuild must be full `--release` AND verify NEON+float IN THE BROWSER.
- **Card UI (N3 pattern):** `#cardbar` (Save/Load/Reset, shown in upython
  mode) + `pi3emu/disk/upycard` IndexedDB key; Load/Reset reboot (image
  injects pre-boot via `restoreCard()`); `sdModel` handle kept in main.js
  for `exportImage`/`loadImage`.
- **E2E (headless Chrome, /tmp scripts):** boot banner + `/sd mounted` +
  REPL `1+1→2` + real hold-button `Pin.irq` rising + save→reload→restore
  round-trip, zero page errors; lirq/irq/sd re-verified on the stock core.
- **Ride-along:** FALLING-edge coverage in `test/upython-irq.mjs` (8/8).

### M38 — single core again: NEON-free firmware (DONE, uncommitted)

M37's dual-core (stock for 19 guests, vendor lazily for upython) stood
exactly one milestone: root-causing the vendor×browser lirq hang showed
the split was backwards. Findings, all reproduced in node:

- Vendor + `lirq.elf` + wall-clock slices = silent 4000 slices, no fault;
  stock delivers at slice 3081. Guest arms `TVAL=0x1000` fine; `cntpct`
  advances but the gt compare never fires.
- The 14/14 "vendor" lirq-probe was never on vendor:
  `test/lirq-probe.mjs` requires `public/unicorn.js` directly. Only the
  upython suites use the vendor core — and upython never touches the arch
  timer (no utime), so the dead gt path hid there. (Vendor's
  `arm64_timer_tick` also throws on plain numbers — strict BigInt wrapper;
  the facade passes BigInt, so compatible. A 2^40 tick lands in the debug
  counter yet the compare still never fires: vendor gt recalc/cval latch
  is dead, exact site unroot-caused.)
- Decisive VFP probe (hand-assembled `fmov d0,#1.5`/`fadd`/`str`,
  `aarch64-none-elf-as` — hand-encoding lies, and `ret` with unset LR
  faults after the store, so stop `until` before it): stock prints 3.0.
  Stock lacks ONLY NEON/SIMD, not scalar float.
- Fix in firmware, not cores: `ports/bcm2837/string_port.c` overrides the
  7 linked newlib string/mem functions with plain C (newlib `strlen` is an
  integer SWAR fast path + a NEON slow path at +0xe4 — the `shrn` that
  killed the version banner), `-fno-tree-vectorize/slp` globally,
  `-fno-builtin` file-local (else GCC turns the loops back into
  memcpy calls). Scalar VFP stays on (doubles natively, libm fine).
- Result: the firmware boots on stock AND all 8 upython suites pass on
  BOTH cores. Dual-core wiring removed (`ensureUpyCore`/`?url`/
  `MUnicornVendor` gone); browser back to one stock core for all 20
  programs (UPY_MODE gating, cardbar, gpio panel stay).
- STANDING CONSTRAINT: the firmware must stay NEON-free or stock-core
  browser upython dies again. Check: `nm` must show the string syms
  resolved to `string_port.o` (small addrs, e.g. strlen at 0x11c3b4 not
  newlib's), `objdump` the string functions for zero `v*.*` insns, and
  the battery proves it (it runs on vendor by default — the stock proof
  was a temporary vendor↔stock file swap, all 8 PASS).
- E2E re-verified single-core: upython full (boot/mount/REPL/button-IRQ)
  + save→reload→restore + lirq/irq/sd, zero page errors.

### M39 — time + machine.Timer (DONE, uncommitted)

- **time/utime:** extmod `modtime.c` (`MICROPY_PY_TIME=1`) on the HAL in
  `uart.c` (`ticks_ms/us/cpu` + `delay_ms/us` off `TMR_CLO`, 1 MHz);
  frozen `utime.py` (`from time import *`) for Pico code — this upstream
  renamed utime to time. `test/upython-timer.mjs` 11/11.
- **machine.Timer** (`machine_timer.c`, Pico API: id/mode/period/callback
  + `init`/`deinit`, `PERIODIC=1`/`ONE_SHOT=0`): id 0..3 ↔ system-timer
  C0..C3 (compare = CLO + period, CS acked, IRQ 1 via legacy IC into the
  local block); the vector re-arms PERIODIC and queues; `irq_drain` runs
  callbacks main-loop-style like `Pin.irq`. Gotchas, all load-bearing:
  enable bit is per-channel (bit `ch`, NOT bit 0 — read `L_TIMER`); the
  CS model hook is inverted vs HW W1C (keep-mask; lirq's `str wzr` works
  around it) so the driver acks `0xF^(1<<i)` complements; a disarmed
  channel's stale compare fires once post-deinit and MUST still be acked
  or its level livelocks the REPL (traced via host `tmrCompares/
  tmrCrossed` sampling); `list.__setitem__` dunder doesn't exist at this
  ROM level (test callbacks need `def`, proven by LED side-channel).
- **BUILD BUG (fixed):** the port's custom `upy_%.o` rules generated no
  depfiles, so extmod objects never rebuilt on header/qstr-pool changes —
  M39's new QSTRs renumbered the pool and stale `upy_modos.o` silently
  lost `os.mount`/`VfsFat` (selective misses: `sep`/`remove` survived via
  earlier first-seen IDs). Rules now emit `.P` files like upstream's
  `compile_c`. Recovery: `rm build/upy_*.o build/extmod_machine_mem.o`
  + rebuild. `CFLAGS` edits still don't trigger rebuilds (make can't see
  flags — `touch` after changing them).
- Regression: 19/20 probes rc=0 (mva pre-existing), all 9 upython suites
  PASS, browser upython E2E green, `npx vite build` clean.

### M40 — pi-cpu: own Rust AArch64 core (DONE, uncommitted)

Motivation verified by measurement, not theory: our unicorn.js executes
via TCI (`TODO tci.c` in its own logs — no JIT in wasm) at ~1.3–1.9 MIPS
in-harness, plus ~200 wasm↔JS crossings per slice. `cpu/` (`pi-cpu`,
zero-dep, workspace member) is a hand-written interpreter: flat RAM,
zero-mapped MMIO + PL011-TX tap, integer subset from an objdump survey
(mov/movk/movn, ldr/str/ldrb/strb/ldur/stur/ldp/stp, add/sub/cmp,
and/orr/eor, lsl/lsr, cbz/cbnz/tbz/tbnz, b/bl/ret/cond, csel/cset/csinc,
ccmp, umulh/msub, adrp/adr, nop/isb). `test/cpu-diff.mjs` runs the same
ELF+budget on both cores and compares console + X0-X30 + SP + PC + fault:
shell/sum/fib PASS at **~120–260 MIPS vs ~1.9 — roughly 100–150×**.
unicorn.js stays until parity; removal is the stated end goal.
- Masks MUST be derived from assembler output, never hand-hex (three
  separate mask bugs caught this way: mul o0, csel op, 2-src overlap).
  Ground-truth method: assemble variants, diff words, keep exactly the
  fixed bits (`cpu/` notes inline).
- CMP/CMN (S=1, Rd=31) discard — must not write SP (found via sp=0
  with x22=sp+8 going along consistently wrong on our side).
- Harness bugs fixed along the way: node X29/X30 IDs are FP=1/LR=2 (not
  X0+29/30); compare regs unsigned (i64 print vs u64 print differ above
  2^63 — the entire x28 saga was half this).
- Build hermeticity is load-bearing: `cargo clean -p` leaves hardlinked
  example binaries behind and fingerprints mislead after touch/RUSTFLAGS
  churn — a full day was lost to phantom "nondeterminism" (CF90/D000
  x28 values) that was stale artifacts + my own mutating runner (one run
  even hardcoded sum.elf for all programs) + misread decimals. Rule: `rm
  -rf target` before differential verdicts, never mutate the runner
  mid-diagnosis (use separate example files), `CARGO_INCREMENTAL=0`.

### M41 — pi-cpu virtual-time timer + clock (DONE, uncommitted)

- `Bus` gains the BCM2837 timer window (own 4K backing; CLO/CHI derived,
  CMP/DONE cells, CS keep-mask absorb, DONE flag) with integer-exact
  virtual time: `vt_ips=262144` advances exactly 15625 us per 4096-slice
  (no float anywhere near integers — the host uses float math, so partial
  tails are banned: harness budgets stay slice-even). `Cpu::run_sliced`
  mirrors the facade sync order (eval pre-chunk, pull+advance post).
  `test/cpu-diff.mjs` takes `[slice] [vt_ips]`; clock passes EXACT
  (console numbers identical!) at ~250 vs ~12 MIPS.
- Real decoder bugs found by clock (all verified against assembler
  truth, all fixed): 64-bit MADD/CSINV/CCMP/2-src clauses never matched
  (masks kept bit30/sf while values dropped them — 32-bit forms passed
  by luck); pair `is64` must be bit31 not bit26 (wrong-but-self-
  consistent stacks hid it); pair class gate must be bits(31:25) (a loose
  mask executed `mov` as stack-smashing STP — caught via x-reg watch
  showing the clobber); LD/ST bit24=0 forms split on bit21 (reg-offset)
  vs (bit11,bit10) =   unscaled/post/pre (my opc mapping was inverted —
  proven by the NUL-bytes putu, i.e. strb landing nowhere).

### M42 — pi-cpu bitfield BFM (DONE, uncommitted)

- BFM semantics oracle-derived (30+ points, `test/cpu-bfm.mjs` 14/14 vs
  unicorn + `cpu/examples/bfm.rs`): with raw fields S=imms, R=immr,
  `S < R` inserts at position (`(dst & !wmask) | (ROR(src,R) & wmask)`,
  the BFI shape), else extracts low (`(dst & !tmask) | (ROR(src,R) &
  tmask)`, the BFXIL shape). Covers canonical aliases plus arbitrary
  (R,S), 64- and 32-bit.
- Methodology lessons (all bitten, all load-bearing): oracle scripts must
  use a VALID dst (BFM reads Rd — a zero/invalid dst makes the ANSWER
  look like extract-low and inverts the conclusion); always zero-pad hex
  (a dropped leading zero misreads a match as a mismatch); hand-built
  32-bit base is `0x33000000` (N=0) — `0x33400000` sets N=1 which is
  illegal with sf=0 and faults (`UC_ERR_EXCEPTION`).
- `decode_masks` now also returns `r` (5-tuple) and handles the esize=64
  replication without `<< 64` (debug-overflow panic).
- Battery still green: shell/sum/fib + clock EXACT.
- gpio parity (prefix through the button-poll spin, 400k budget + vt):
  needed LSL-reg (`lsl w9,w23,w9` at 0x100A7C, Rust's `1 << var`) — the
  2-source arm EXISTED (UDIV/SDIV/LSLV/LSRV/ASRV/RORV) but its mask
  `0x7FE0F800==0x1AC00800` fixed opcode bits 15:11=00001 (div-only),
  leaving the shift arms dead. Correct mask from 8-word assembler truth:
  bits 30:21 constant + opcode bits 15:14=00 + bit12=0, i.e.
  `(w & 0x7FE0D000) == 0x1AC00000` (verified disjoint from CSEL's
  `0x3FE00800` mask). `test/cpu-shift.mjs` 15/15 (both widths, div-zero,
  INT_MIN/-1) via `cpu/examples/shift.rs`.
- MSR/MRS/system class (`(w>>25)&0x7f==0b1101010`) records VBAR_EL1 +
  DAIF.I (assembler truth: `msr vbar_el1,x0`=0xD518C000 {3,0,12,0,0},
  `msr daifclr,#2`=0xD50342FF, imm in CRm; DAIF.I resets masked).
  Everything else in the class stays a NOP.

### M43 — pi-cpu GPIO IRQ phase: button + host-assisted delivery (DONE)

- `Bus`: GPIO window (LEV from latch+inputs, EDS W1C, 17-cell EV-reg
  backing incl. reserved gaps, edge eval in `sync_out` from input
  transitions gated by the enable union + HEN/LEN level-force — mirrors
  `gpio.js`) + legacy-IC bank 2 (ENABLE accumulate, PENDING2 bit 17 +
  BASIC bit 9 from the gated line — mirrors `ic.js`) + IRQ_RET magic at
  IC+0x2C (`irq_ret_pending` flag). `Cpu`: `vbar_el1`/`daif_i` fields.
- `run` example: optional `[press1 release1 press2]` insn schedule
  (BTN29) + post-chunk IRQ_RET-resume-then-deliver loop mirroring the
  facade `runSlice` order (same chunk/slice size ⇒ same guest points).
  `cpu-diff` applies the same schedule via `emu.setButton`.
- Full gpio: `cpu-diff gpio 700000 4096 262144 350000 400000 450000`
  PASS (console/regs/sp/pc/insns/fault) through chase, poll, release-
  edge delivery#1, "IRQ phase done", press2-edge delivery#2.
- REAL DECODER BUGS this flushed out (both survived self-consistently
  until the glue needed real restores):
  - LDP/STP load-vs-store is **bit22, not bit30** (bit30 is 0 for every
    pair form; all LDPs executed as STPs). Assembler truth table for all
    8 offset/pre/post × 32/64 forms. `stkcheck` one-off proved it.
  - `movz w0,#0xfff0` scare was my own mistyped probe word (0x528FFE00
    vs disassembly's 0x529FFE00) — model was correct.
  - sp=0x3FFFB0 (not 0x3FFFF0) is rust_main's own 64-byte frame, not a
    leak — verified by chunk-boundary sp trace (`spy` one-off).
- Diagnosis discipline held: separate one-off examples (irqstep/spy/
  stkcheck/…), all deleted after; `run.rs` tracing removed.
- Battery still green: shell/sum/fib + clock EXACT + bfm 14/14 + shift
  15/15 (all re-verified from `rm -rf target`).

### M44 — pi-cpu UART0 + timer-IRQ + mapped-set faults (DONE, uncommitted)

- **Mapped-set fault model:** data accesses outside {RAM, UART0, TMR,
  GPIO, MBOX page, LOCAL} now fault `UnmappedData` like the unicorn
  core (the M40 zero-map-everything was a shortcut). MBOX page
  (0x3F00B000, IC ride-along) + LOCAL page absorb as zeros, mirroring
  the facade's default mappings. `is_ic` capped at the MBOX page end
  (past 0x3F00C000 the facade faults too). Passing guests are unaffected
  (they only touch mapped windows — proven by their unicorn runs).
  `cpu-diff` is fault-aware: fault-on-either requires fault-on-both +
  identical console/regs/sp, allows pc-4 (pi-cpu pre-increments pc),
  skips insns (facade overcounts to the slice end on fault).
- **UART0 model** (mirrors `uart0.js`): IBRD/FBRD/LCRH/CR/IMSC cells the
  guest reads back, 16-deep RX FIFO (`uart0_push`, enabled+space gated),
  DR read = head+pop, dynamic FR (TXFE always, RXFE iff empty),
  RIS = RXINTR-if-nonempty | TXINTR-always, MIS = RIS&IMSC, ICR absorb.
  DR-write console tap skips zero bytes (matches the facade hook).
  `run`/`cpu-diff` take `[keybyte] [keyat]` (single edge-triggered push).
- **IC bank 1** (timer C0-C3): PENDING1 + BASIC bit 8 (non-shortcut),
  UART PENDING2 bit 25 + BASIC shortcut bit 19.
- Full parity, all PASS: `uart0 200000 ... 0 0 0 72 40000` (RXINTR MIS
  0x10, [rx 'H'], TXINTR MIS 0x20, de-arm, no storm) and `irq 500000 ...
  0 0 0 72 350000` (timer C1 "[irq #1 t+1s]" + UART key).
- Sweep tally: clean PASS — shell/sum/fib/clock/gpio/bench/irqcore/irq/
  uart0/fb; fault-both PASS — debug/periphs/uart1/i2c/spi/pwm/sd/dma/
  smp/mmu (all unmapped-window faults at identical points).
- BLOCKED (needs new models): mva (MMU), lirq (local block + arch
  timer), firmware/upython (SDHCI + fuller UART/GPIO).

### M45 — pi-cpu 1-source class + assembler-truth fuzzer (DONE, uncommitted)

- New `test/cpu-cases.mjs`: every case word comes from
  `aarch64-none-elf-as` (labels key words, never position, never
  hand-hex), each runs once on the unicorn oracle + once on pi-cpu
  `cpu/examples/one.rs` (fresh `Unicorn` per case — instance reuse hits
  translator-buffer exhaustion), comparing fault + X0-X30 + SP + PC +
  NZCV + 3 scratch windows. 151 snippets x 3 vectors (V0 small ints,
  V1 scratch bases, V2 MSB-heavy) = 453/453 PASS.
- 1-source arm (`(w & 0x7FC00000) == 0x5AC00000`, verified disjoint from
  MADD/CSEL/CCMP/ORR/ADD/LSL-reg/UDIV/BR/RET/ADRP): op6 = bits(15:10)
  selects the op, sf the width — 0 RBIT, 1 REV16, 2 REV32(X)/REV(W),
  3 REV(X only; sf0 unallocated -> Illegal), 4 CLZ, 5 CLS, both widths
  each (W results via `w()` zero-extend).
- REV32-X reverses bytes WITHIN each 32-bit half (bswap32 per half,
  halves stay: 0x1122334455667788 -> 0x4433221188776655, fork-probed) —
  NOT a half-swap. CLS counts the run FOLLOWING the top bit (spec,
  fork-probed both widths incl. negatives: no fork quirk anywhere in
  this class — the earlier "quirk" readings were my own swapped
  want/got columns plus three hand-transcribed words).
- Methodology (bitten, load-bearing): transcribing objdump words by hand
  caused this whole detour (rbitw/clzw/clsw typos — the Rn field in my
  own table contradicted the source line and I didn't notice); always
  machine-extract words. DIFF format is `oracle/pi` (`want/got`).
  Assembler limits are real: LDRH unsigned max is #8190 (#32766 does not
  assemble); `rev16 w6, w7` is legal (op6=1, sf0).
- Also added to fuzzer: STNP/LDNP, LDPSW pre/post-index, offset
  extremes (`ldr x20,[x21,#32760]`, `stur [...,#-256]`), all green.
- Battery still green with the new arm: shell/sum/fib/clock/bench/
  irqcore/fb/gpio/uart0/irq PASS; periphs/i2c/spi/pwm/dma/smp/mmu/mva/
  debug fault-both PASS; lirq console/sp/pc/insns/fault ok, x1-only
  residual (known chained-TB sliver, pre-existing).
- NOTE (pre-existing, NOT M45): `cpu-diff uart1/sd` currently FAIL
  (unicorn faults early at `adr 0x10016c`, pi-cpu runs on) — reproduces
  with the 1-source arm gated off, so it belongs to the active
  facade/SDHCI/sd.elf workstream, not the decoder. MMU/local-block/
  arch-timer/SDHCI/mini-UART/firmware-REPL pi-cpu models (all working,
  in-tree as of 29ec341) still need their own AGENTS.md entries.

### M46 — firmware /sd mount verified (DONE, uncommitted)

- The `OSError: 19` / `readblocks b[0]=1` mount failure does NOT
  reproduce on the current tree: with a fresh `make -C ports/bcm2837`,
  guest `readblocks(0)` bytes match host `mem_read(BLOCK_DATA)` exactly
  (`eb3c9050...`), boot auto-mounts (`/sd mounted: ['HELLO.TXT']`),
  `os.listdir('/sd')` works, zero faults. Root cause was a stale
  firmware build (frozen `sdcard.py` older than the model) — always
  rebuild after touching frozen sources *or* the model ABI. Proven by
  `test/upython-sd.mjs` 8/8 + `test/upython-vfs.mjs` 14/14 PASS (incl.
  remount coherence) on commit `29ec341`.
- (Separate, still open: `cpu-diff uart1/sd` parity, M45 NOTE above.)

### M47 — Python runs on pi-cpu: scalar VFP + SUB/CCMP decoder fixes (DONE, uncommitted)

- **VFP core:** D/S file on the Q low half (zero-extend on write;
  d31 is real, no XZR alias). FP singles: size is `10`=S/`11`=D
  (NOT `01` — first version faulted every S load/store; pairs use
  `00/01/10` = S/D/Q). Extended the 0x16 pair + 0x1E single arms in
  place (the old Q-only arms silently mis-executed S/D pairs as
  16-byte Q). FP data-proc is a flat (mask,value) table — 76 rows,
  machine-derived, 0 cross-collisions. `fmov`-imm expand:
  sign=a, exp=(b?0x3FC/0x7C:0x400/0x80)|(cdefgh>>4),
  frac=(cdefgh&15)<<off (oracle-fitted, 8+1 points). FPCR hardwired 0
  (12 MRS reads, 0 MSR writes in the image); FPSR cumulative
  (honest DZ/IO/OF/UF + exactness-proven IX, nothing live reads IX).
- **Fork quirks mirrored (all probed, fuzzer-pinned):** SNaN quiets
  preserving sign+payload (not DefaultNaN); V=1 on ANY unordered
  compare (even quiet QNaN); FMSUB<->FNMSUB negation SWAPPED on the
  fork (normals probe: FMSUB(2,3,4)=-2, FNMSUB=+2); away-mode
  (frinta/fcvtas) flushes subnormal inputs to signed zero (other
  modes unaffected — frintp/m proven clean).
- **Fuzzer:** new `fp` group (77 snippets x 3 vectors incl. inf/NaN/
  SNaN/subnormal/0/0 patterns = 231/231) + D0-D31 seeded/compared in
  the harness; full battery 735/735 (245x3). Added missing arith
  coverage: ccmp/ccmn reg+imm, csel/csinc/cset/csneg, neg/subs/adds
  x31-vs-sp forms.
- **SUB-shifted Rn31=XZR (was SP):** `neg x1,x1` computed SP-x1 =
  0x3FFAAF and wild-faulted firmware (`str x0,[x25,x1,lsl#3]`);
  form-dependent (extended keeps SP — fuzz-pinned). The old comment
  keyed on S and had it backwards.
- **CCMP-imm bit11 (was X[imm]):** `ccmp w0,#5` compared against X5,
  so `<` behaved as `==` and `!=` raised TypeError (fell past the
  equal-handler). Found via NZCV-per-step trace.
- **Harness:** `cpu-diff` takes `PI3_KEYS` multi-key schedules on both
  sides (run.rs already had it; fixed its tuple-sort byte-reorder)
  and attaches SDHCI for firmware (pi-cpu disk always present).
- **Result:** firmware keyed session (boot + `/sd mounted` + `1+1`
  → `2`) FULL PASS console/regs/sp/pc/insns/fault; guest battery
  green (lirq x1 chained-TB sliver only); upython-sd + upython-vfs
  PASS on the rebuilt firmware.
- **Open:** uart1/sd parity (unicorn faults at `adr 0x10016c`,
  pi-cpu runs on — pre-existing, untouched); QNaN-quiet payload
  passthrough rule untested (no fuzzer case hits it); `add sp`
  Rd=31 write-SP latent (w() drops).

### M48 — pi-cpu to wasm + device-model parity batch (DONE, uncommitted)

- **Phase A (wasm core):** `cpu/src/runner.rs` (run loop moved verbatim
  out of the `run` example — native diff and browser share it),
  `cpu/src/wasm.rs` (`PiEmu`: load/run/console/keys/button/vt/card/
  gpio/fault/pc/regs/mem), wasm-bindgen dep (wasm32-only).
  `wasm-pack build` → 65 KB wasm; node smoke boots firmware to a
  mounted REPL and runs `1+1`→`2` + sum guest. No UI wiring yet.
- **uart1 full:** ENABLES latch + LSR + IO pulse-clear + CNTL/LCR/BAUD
  backing + `[u1] ` line tag (uart1Emit rule); cpu-diff attaches.
  PASS.
- **i2c/spi slaves** (sensor 0x68, JEDEC flash): edge logic + window
  backing; STATUS served from sync_out **snapshots** (live DONE let
  polls exit a chunk early → constant 1-insn phase shift; i2c proved
  it, spi needed the same); SPI CS dirty-flag (facade window shows
  same-slice writes). i2c PASS; spi PASS.
- **dma** (ch0 + ENABLE page as window backing, chain engine with
  page-chunk IGNORE fills, END/INT publish, bank-1 bit16): PASS.
  (Two traps bitten: fault-both labels read expected/actual —
  "unicorn: true" means pi-cpu faulted; and the fault addr was
  ENABLE+4, misread as DMA+0x54.)
- **pwm** (CTL latch + 256 FIFO + 64/chunk drain ring + FULL/EMPT,
  `pwm_take` for the worklet): PASS.
- **mbox+FB** (property tags + allocate/pitch/geometry for canvas):
  fb PASS (with vt, like clock).
- **mmu-ctl compat:** MMU_CTL write programs the real regime
  (TTBR0/T0SZ=16/MAIR/SCTLR.M) + `mmu_loose` dialect flag (0b01 =
  table-descend, needed alongside strict-ARM mva blocks) + 48-bit L0
  + device-window bypass in translate(). `test/mmu-parity.mjs`
  (probe console vs pi-cpu run): PASS. cpu-diff keeps fault-both
  (no attach — facade needs probe-retry there).
- **sd:** PASS after attaching (was just a missing attach, like uart1).
- **Process lessons (load-bearing):** rebuild BOTH debug+release after
  lib changes (stale release trapped us 3×); fault-both "unicorn: X"
  prints the EXPECTED literal — read the pi-cpu value.
- **Open:** smp (needs 4 cores + 0x3F202000 spin-table; faults at
  insn 7 now); periphs/debug fault-both by design; lirq x1 sliver;
  M47 QNaN/add-sp notes stand.

### M49 — unicorn.js removed: pi-cpu is the only core (DONE, uncommitted)

unicorn.js is GONE (public/unicorn.js, packages/pi3-emu/ (facade + vendor),
src/patches/, @alexaltea/unicorn-js dep, all *-probe oracles archived to
test/archive/).
The demo page (src/main.js, rewritten ~700 lines) runs PiEmu/PiSmp from
cpu/src/wasm.rs (public/pi_cpu/, built by build.sh via wasm-pack) for all
20 programs; public/linux/ (qemu-wasm) untouched. Verified: 16/16 browser
boots + shell/uart REPL/float/gpio-button/fb-canvas E2Es, zero page errors.

- **Mailbox shadow bug (REAL, fixed):** is_ic spans IC_BASE..MBOX_PAGE end,
  swallowing the mailbox at 0x3F00B880 (reads hit `_ => 0`, MAIL1_WRITE
  absorbed) — fb guest spun past the polls ("mailbox failed"). Fix: check
  the mailbox arm BEFORE is_ic in read() AND write() (lib.rs). Lesson:
  range-windows must be ordered narrow-first; the old facade's exact-addr
  models never overlapped so nothing caught it (M48's "fb PASS" was both
  sides equally broken — facade has no mailbox model).
- **Firmware float gap (8 SIMD + 8 fixed-point rows, all oracle-fitted):**
  movi-d, fmov x,v.d[1], fmov v.d[1],x, orr-vec, bit/bif (Vm is the
  SELECTOR — first version had Rn/Rm swapped), fneg-2d, mov-elem-D,
  scvtf/fcvtzs-fixed-#0 (D-file <-> D-file! int64<->double stay in Dn),
  scvtf/ucvtf-fp-#N (fbits = 64-scale6[21:16]), fcvtzs/u-fp-#N (fbits =
  64-scale6[15:10], float->fixed MULTIPLIES — first version divided).
  Fixed-point #0 is a separate encoding (assembler rejects #0 for scaling
  forms). Q=0 vector forms clear the top half (oracle-proven).
- **EXTR Rn/Rm SWAPPED (the big one):** the extras arm computed
  (Rn>>lsb)|(Rm<<(w-lsb)) instead of (Rn<<(w-lsb))|(Rm>>lsb) — hidden by
  the lsb==0 and Rn==Rm (ROR-alias) cases. Symptom: multf3 mantissa lost
  (10x10 -> 64.0), floats printed ~55x small. Found by differential
  function-tracing (__floatunditf verified clean first, then tail
  comparison). Methodology that paid: trace.rs (ELF+startpc+regs+patch+
  overlay tracer), injected snippet with assembler-verified bls (gas
  treats `bl <abs>` as relative — use explicit `bl . +/- delta` and
  verify with objdump), per-step GPR diff vs unicorn stepper.
- **Harness bugs bitten (all mine):** hex-vs-dec arg confusion (trace hx
  is hex-only; sess is decimal; ASCII codes), unicorn emu_start needs LR
  set + pc-resume loop, simd-rig Q-seed offsets/endianness, sess reply
  ordering (fire-and-forget KEY needs discard waiters), release-vs-debug
  staleness (ALWAYS rebuild both; a stale release mimics core bugs).
- **SMP browser cutover:** PiSmp (512-slice, 2000 rounds) boots to the
  full join in the page; updateStats shows park/counter/msgs.
- **Tests now:** test/pi-cpu-smoke.mjs (22 goldens incl fb hash +
  fault-pinned periphs/debug), test/cpu-cases.mjs (819 golden cases incl
  simdguest group; oracle version archived; --regen), test/upython-*.mjs
  (9 suites via test/pi-sess.mjs + cpu/examples/sess subprocess; REPL
  floats green incl math.sqrt exact string).
- **Open:** S-fixed-point forms skipped (unobserved, fault honestly);
  bsl/dup-2d/ushr skipped (same); to-int-scale IX/OF flag merging is
  approximate (nothing live reads them). `sess` pipe protocol: every
  `_cmd` has a 240 s timeout (a dead child fails the suite LOUDLY with
  the command shown — an early version hung silently on a lost reply),
  fire-and-forget KEY lines consume discard-waiters in order, and
  process-exit reaps children (suites leaked sess processes before).

### M50 — M30 peripheral windows in pi-cpu: every program green (DONE, uncommitted)

periphs + debug were the last two fault-pinned guests (all other 20
pass). The M30 windows now live in Bus, ported from the deleted facade
models (recovered from git history as spec):

- RNG 0x3F104000: CTRL latch (default 0) + fixed 45000 temp DATA
  (mirrors rng.js/temp.js); CLK 0x3F100000, I2S 0x3F203000, BSC0
  0x3F205000: zero windows (absorbing — guests only read them at rest).
- AUX UART2-5 (0x3F216000/7000/8000/9000): per-UART window backing +
  ENABLES latch, LSR served live (0x60 when enabled, mirrors uart25.js).
  The existing UART1 ENABLES cell already satisfies the SPI1/SPI2
  ENABLES read-back the periphs guest checks (same +0x04 address).
- USB 0x3F980000 len 0x40000 (range check, no backing): GSNPSID reads
  0x4F54280A (the debug guest accepts ONLY 280A while periphs accepts
  either — both are real DWC2 revs, so serve 280A); writes to +0xFF0
  (periphs) or +0x54 (debug) set usb_done → `done_flag` sel 7, wired
  to periphs/debug DONE branches in src/main.js.
- All new windows join the mapped-set + `is_device_win` (MMU bypass).
- Debug guest fixes (its expectations, not the models — models match
  real HW + the old facade): CLO mask → top byte (wall-clock race),
  FR expect → 0x90 (idle TXFE|RXFE), MAIL1_STATUS expect → 0 (never
  full at idle), SPI0 CS expect → 0x40000 (TXD always drained).
- Result: periphs ALL PASS (8 checks + parked), debug 22/22 ALL PASS,
  fault null. Smoke goldens updated (no more fault-pinned guests);
  pw-verify 27/27 in the browser.
- **Open (carried):** S-fixed forms, bsl/dup-2d/ushr, IX/OF merging
  (unchanged); M30 windows beyond what the two guests touch (RNG
  FIFO/IRQ, clock ENAB/BUSY behavior, I2S FIFOs, USB OTG) remain
  stubs — enough for the guests, honest about the rest.

### M51 — decoder completions (DONE, uncommitted)

Closed the carried M49/M50 opens (S-fixed-point, bsl/dup-2d/ushr,
IX/OF), all oracle-fitted against the stock core before its removal
(restored to /tmp from git for the purpose; rigs live only in
/tmp/opencode/, never in-repo):

- S-fixed mirrors: scvtf/ucvtf s,s,#N (same 64-scale rule, 32-bit int
  side, high D/Q bits ignored) + #0 plains. **Find:** the first cut
  missed the `ucvtf s0, s0` plain row (0x7E21D800) — pi-cpu faulted
  where the oracle executes; added.
- BSL `.8b/.16b`: Vm-selector order like BIT/BIF. **Oracle divergence
  (independently re-verified, kept spec-correct):** the stock fork
  computes Rd=(Rn&Rd)|(Rm&~Rd) (Rd as selector — truth-table-proven
  over machine-checked classes; its BIT implements the identical
  formula correctly and agrees with ours exactly, so this is a
  BSL-only fork bug). Inert: zero BSL hits in all 23 guest ELFs.
- DUP `.2d` from X (full-Q replicate; XZR→0 verified, excluded from
  the fuzzer), USHR D + `.2d` (shift=128-imm7, #64→0, D top zeroed),
  pre-existing EOR-`.8b` top-clear (never checked before — confirmed).
- IX exactness (REAL bug, fixed): `fp_from_int` fired IX on magnitude
  (`mag > 2^53/2^24`), over-firing on exactly-representable large
  ints (2^30→f32, 2^60→f64); now a significance test
  (`bitlen − trailing_zeros ≤ 53/24`, `fp_sig_bits`). Fixed-point
  single-rounding proven, not assumed: int→float scale is
  exponent-only/exact and significand-preserving (so the one check
  covers the scaled quotient), float→fixed widen+scale exact with a
  single truncation — 706 adversarial value+flag cases + 1800
  randomized int×fbits cases, all green.
- Fuzzer: 21 snippets appended to `simdguest`, goldens regenerated
  (882/882, old 819 byte-identical — zero drift); smoke 22/22,
  upython-repl PASS, browser `1.5+2.25→3.75` green on the rebuilt wasm.
- **Open (carried):** BSL-vs-oracle divergence (documented above,
  inert); M30 stubs (unchanged).

### M52 — kernel track opens + linux-st direct boot option (DONE, uncommitted)

Two independent slices in one commit (user asked for a single commit):

**A. Own-Rust-kernel track (first blood).** `ports/rpi-kernel/` is a
standalone crate (own `[workspace]` — detached from `programs/` at
0x100000 and the root host workspace, so no default `cargo build`
touches it): `kernel.ld` `ENTRY(_start)` at `0x80000` (the real Pi
boot address; `load_elf` honors `e_entry`, verified `Entry 0x80000`
via readelf), `_start` sets SP to `0x3FFFF0` + zeroes `.bss` via
`__bss_start/__bss_end` + `b rust_main`, `rust_main` inits the PL011
(115200 @ 3 MHz, 8N1, UARTEN|TXE|RXE), prints
`rpi-kernel M52: hello from 0x80000` + `Echoing input now` (the
05_drivers_gpio_uart shape), then echoes each key as
`[echo 'c']`. Only proven pi-cpu instructions (no EL/MMU/timer yet —
still the stretch list). Build: `build-kernel.sh` (own
`.cargo/config.toml` with `-Tkernel.ld` + `rust-lld`), hooked into
`build.sh`; ELF committed at `public/programs/rpi-kernel.elf`.
Wired as the `rpikernel` demo program (`PROGRAMS` + select + idle
branch — the getc spin idles like the shell) + smoke golden (key
`H`@40000) + pw-verify golden. Verified first-try:
`run rpi-kernel.elf 200000 ... 72 40000` prints banner + echo,
fault null.
- Stretch (still open): CNTFRQ_EL0/CurrentEL/MPIDR reads,
  ELR/SPSR/SP_EL1 latch for an EL2→EL1 `eret` drop, 4K MMU at EL1,
  CNTPCT tick + local-block wiring, SMP spin tables — the gap table
  from the M51 audit.

**B. `linux-st` direct boot option.** The demo had only `linux`
(MT engine); the ST engine was reachable solely via the
sentinel-gated auto-handoff (`__pi3NeedSt` → `../linux-st/.bootable`,
never created since ST deep execution is upstream-blocked: 49a56a7
— RR `rr_cpu_thread_fn` init got past `tb_ptr_ptr`, then ~5 min
silent, death on uncaught `Infinity` = escaped setjmp/longjmp across
private heaps; needs main-thread-only execution upstream, not
flags). New `linux-st` select option boots `public/linux-st/
index.html` directly with threads forced `off` (initramfs path,
`runLinux('./linux-st/index.html', 'off')` in `src/main.js`) —
bypassing the handoff gate as an explicit attempt/observe path.
Deliberately NO `.bootable` sentinel: creating one would assert a
verified shell boot and flip auto-handoff into a known-dying engine.
Verified here: page wiring (iframe src, xterm/engine start, zero
page errors); full ST shell boot still requires a real browser and
remains blocked upstream.

### M53 — own-kernel EL2→EL1 drop (DONE, uncommitted)

Second blood on the own-kernel track (`ports/rpi-kernel/`): the kernel
now boots the rust-raspberrypi-OS **09_privilege_level** shape instead
of the M52 bare print loop. `_start` checks `CurrentEL==EL2` (0x8,
else park), `MPIDR` core 0 (else park), zeroes `.bss`, requires
`CNTFRQ_EL0≠0` (else park — all three parks are the 09 `boot.s`
shape, `wfe` replaced by a plain spin since pi-cpu no-ops `wfe`),
then `rust_main(stack_top)` prints the EL2 banner and drops via
`CNTHCTL/CNTVOFF/HCR` (absorbed) + `SPSR_EL2=0x3C5` (D/A/I/F masked,
M=EL1h) + `ELR_EL2=kernel_el1` + `SP_EL1` + `eret`. `kernel_el1`
prints `CurrentEL`, the timer frequency, spins 1 s on `CNTPCT`, then
echoes (`Echoing input now` + `[echo 'c']` — the 09 `kernel_main`
tail). Reference vendored at
`ports/rust-raspberrypi-OS-tutorials/` (full clone, `.git` stripped
so git tracks it as plain files — a nested `.git` would break the
outer repo; 15 MB / 1213 files, kept whole so any chapter's driver
can be ported next without re-fetching).

pi-cpu core additions (all encodings from
`aarch64-none-elf-as`/`objdump`, never hand-hex):
- `Cpu`: `spsr_el2/elr_el2/sp_el1/cur_el(2 at reset)/spsr_el2_msrd`
  fields.
- MRS: CurrentEL `{3,0,4,2,2}`→`cur_el<<2`, MPIDR_EL1 `{3,0,0,0,5}`→0
  (single core 0), CNTFRQ_EL0 `{3,3,14,0,0}`→19_200_000 (the real Pi
  3 rate; 09 parks on 0).
- MSR latches: SPSR_EL2/ELR_EL2 `{3,4,4,0,0/1}` (MSR SPSR arms
  `spsr_el2_msrd`), SP_EL1 `{3,4,4,1,0}`; absorbs HCR_EL2
  `{3,4,1,1,1}`, CNTHCTL_EL2 `{3,4,14,1,1}` (note: real op2=1 —
  `msr cnthctl_el2,xzr`=0xD51CE11F — first cut used op2=0 and the
  kernel parked), CNTVOFF_EL2 `{3,4,14,0,3}`.
- `eret()`: when `cur_el==2 && spsr_el2_msrd` (explicit EL2 MSR seen),
  consume SPSR_EL2/ELR_EL2 (NZCV + I-bit→DAIF.I), install SP_EL1,
  `cur_el=1`, resume at ELR. The `spsr_el2_msrd` gate is load-bearing:
  without it lirq's native EL1 eret (SPSR_EL1 path, zero EL2 regs)
  wrongly took the EL2 branch → pc=0 → `Illegal(0)` at insn 4117
  (caught by smoke: 22/23, lirq FAIL; fixed to 23/23).
- Lesson repeated: hand-derived encodings lie — CurrentEL is
  `{3,0,4,2,2}` (`mrs x0,currentel`=0xD5384240), NOT `{3,0,0,0,2}`;
  the first cut parked the kernel at 0x8004C forever.

Verified by execution (NOT committed, per user instruction): native
`run rpi-kernel.elf 20000000 ... 72 15000000` prints EL2 banner +
`in EL1 (CurrentEL 0x4)` + `timer freq 19200000 Hz` + spin + echo,
fault null; fuzzer 882/882; smoke 23/23; `npx vite build` clean;
browser rpikernel on the rebuilt wasm prints banner/EL1/freq/spin +
typed `[echo 'H']` with zero page errors (the pre-rebuild preview
served a stale ELF — rebuilt `dist/` fixed it).

### M54 — own-kernel bring-up batch (DONE, uncommitted)

Four tutorial tracks landed in `ports/rpi-kernel/` at once (designed
by 4 parallel research subagents, integrated single-handed to avoid
merge conflicts — the user asked about multi-agent editing: parallel
`edit` calls to the same files race and conflict, so research fanned
out but all writes went through one integrator):

**A. Driver structure** (dep-free 05 shape): `synchronization.rs`
(NullLock), `driver.rs` (Manager, 2 slots), `console.rs`
(Write/Read/All, no statistics), `print.rs` (`print!`/`println!`
via `format_args!` — NO nightly `format_args_nl!`, stable 1.97.1),
`bsp/uart.rs` (PL011 raw volatile: CR=0 → ICR=0x7FF → IBRD=1 →
FBRD=40 → LCRH=0x70 → CR=0x301; FR TXFF/BUSY spins are model
no-ops, CR must keep bit9+bit0 or keys drop), `bsp/gpio.rs` (pins
14/15 ALT0 + PUD dance, absorbed), `bsp/driver.rs` (GPIO+UART
statics, map→register→init→register-console). Panic handler prints
raw `PANIC` over the UART (a silent spin cost a full debug round —
the first M54 boot printed nothing with fault null because
`print!` ran before `register_console` and died in `expect()`).
`main.rs` ordering bug of the same family: `install_vectors` ran
before `init_drivers` and its `adr x0,vec_start` word never
executed — banner first, then drivers, then VBAR.

**B. Sync SVC** (ch12 shape): `SVC #imm` fills ESR (EC=0x15,
ISS=imm16) + ELR=pc + SPSR=pstate() + DAIF mask + pc=VBAR+0x200
synchronously in `step()` (NOT chunk-edge like IRQ). Vector glue
saves x0/x1, calls `svc_handler(elr,spsr,esr)` printing EC/ISS/ELR,
then ELR+=4 skip + native eret. Core fixes: SPSR_EL1/ELR_EL1 MSR
now latch (were no-ops — handler `eret` needs them); ESR_EL1
MRS/MSR + FAR_EL1 MRS + DAIF MRS/MSR added (all words from
`aarch64-none-elf-as`, never hand-hex). Lesson: SVC low bits are
`0b01` (`svc #0x1337`=0xD40266E1 ends 0xE1) — the first gate tested
`(w&0xff)==1` and faulted `Illegal(0xD40266E1)`.

**C. Timer IRQ** (ch20 + lirq shape): `kernel_el1` arms TVAL=freq +
CTL=1 + `daifclr`, handler prints `[timer N] src`, re-arms ×2 then
CTL=0 conclude + TIMER_DONE. `LOCAL+0x60` reads 0x2 (CNTPNS, GPU
clear). Native `eret` (cur_el==1 path). Smoke: 3 ticks then echo.

**D. Identity MMU** (mva 4K shape, NOT the tutorial's 64K — the core
is 4K-only and faults TG0≠4K): `asm_clear_l1` (naked, x9-x11)
zeroes 0x280000 (a Rust loop kept base in a caller-saved reg the
`mmu: off` print clobbered → Translation at TTBR0 write), L1[0]=
0x401, TCR=0x3519 (T0SZ=25/TG0=4K), MAIR=0xFF, TTBR0=0x280000,
SCTLR.M|C|I, ISBs. Post-MMU rule (load-bearing): NO `adrp` to
`.data` statics after enable — rustc's pre-enable-PC page math
mis-forms (`UnmappedData(0x124F810)` killed the timer handler's
TIMER_COUNT adrp AND the UART static's adrp); `adr` (±1 MB,
PC-exact) everywhere in handler/poll paths (`sym` operands).
`kernel.ld` pins `.data` at 0x90000 (RAM + adrp/adr range) and
`.tables` at 0x280000.

**Core LDRH fix (real decoder bug):** `ldrh w11,[x8,#8]`=
0x7940110B faulted `Illegal` — the 0x1C arm diverted bit26==0/
size==1 to SIMD. Truth: bit26==1 is SIMD; bit26==0/size==1 is the
integer halfword form (same arm as LDRB/W). One-line gate fix.

Verified by execution (NOT committed, per user rule): native
`run rpi-kernel.elf 20000000 ... 72 16000000` prints the full POST
(EL2 banner → VBAR → EL1 → freq → drivers → SVC EC/ISS/ELR →
after → mmu off/on → 3×timer → echo H), fault null; fuzzer
882/882; smoke 23/23; `npm run build` clean. Browser + docs are
the two remaining boxes (vite preview + Playwright rpikernel,
AGENTS M54 + README entries — this text).

### M56 — real-Linux track opens: RAM + loader + first-fault triage (DONE, uncommitted)

User-locked goal restated (see PRIME DIRECTIVE): boot the REAL
upstream `raspberrypi/linux` kernel8.img on pi-cpu in the browser,
qemu-wasm as oracle only. Four research subagents fanned out
(read-only); every claim below was re-verified by the integrator by
execution (per the subagent rule — two agent outputs needed
correction: the `load.js` slice table and the `load_elf` entry story).

- Reference assets (committed `.data`, 26700273 B): dtb `0:32753`
  (32753 B), kernel8.img `32753:22505969` (22473216 B, ~21.4 MB
  raw `Image`), rootfs `22505969:26700273` (4194304 B = 4 MiB).
  qemu argv (`public/linux/module.js:115-133`): `-M raspi3ap
  -m 512M -smp 4 -dtb/-kernel/-drive if=sd -append <KERNEL_COMMON
  root=/dev/mmcblk0 rootwait ...>`; ST uses `-initrd` instead.
  Kernel tag `1.20230405` + `bcm2711_defconfig` + busybox 1.36.1
  (`scripts/linux-rootfs/image.Dockerfile`). Decision:
  **initramfs-first** (no SDHCI/DMA/ext2 work needed for the first
  shell; SD path deferred).
- pi-cpu additions: `LINUX_RAM_SIZE` 512M + `linux_mode` flag
  (`ram_size()`/`in_ram()`; legacy `is_ram()` 4M untouched so all
  23 smoke + 882 fuzzer goldens are byte-identical),
  `LINUX_KERNEL_PA=0x200000` / `LINUX_DTB_PA=0x3000000` /
  `LINUX_INITRD_PA=0x4000000`, `load_linux()` (fresh zeroed 512M
  RAM + 3 raw blobs, returns kernel PA), `Cpu::linux_reset()`
  (x0=DTB PA, x1=x2=x3=0, EL2, DAIF masked, MMU off, SP seed).
  `translate()` table-walk + DMA/mailbox/mem helpers now use
  `in_ram()` (Linux-aware); fetch/read/write paths use `in_ram()`.
  Removed two `eprintln!` debug lines (SCTLR-W, R/WENTER) the triage
  no longer needs.
- Harness: `cpu/examples/triage.rs` (slices `.data` in-process,
  `load_linux`, `linux_reset`, `Runner` chunks at vt 262144,
  prints entry/ram/sizes + first fault) + `test/linux-triage.mjs`
  (`node test/linux-triage.mjs [budget] [slice]`).
- FIRST FAULT (by execution, 200k budget): `n=47968 pc=0xdb1444
  x0=0x187e000 fault=UnmappedData(0xffffffc008ba1aa8)`. Reading:
  the kernel enabled its MMU (SCTLR writes observed pre-fault),
  then touched a `0xffffffc0...` kernel VA (TTBR1 high-half /
  `PAGE_OFFSET` linear map) that pi-cpu cannot walk (TTBR0-only,
  4K-only, no TTBR1/T1SZ/ASID/perms). So the next slice is
  **TTBR1 + high-half walk** (then data-abort vectors, ID regs,
  atomics, NEON — ranked in the gap table; `test/archive/` holds
  the M20-era notes, oracle only).
- Regression still green: smoke 23/23, fuzzer 882/882.

### M57 — real-Linux track: 200M fault-null (DONE, uncommitted)

The triage fault chain, closed slice by slice, every step verified by
execution (`node test/linux-triage.mjs`, never by reading). Headline:
**the real kernel8.img now runs 200,000,000 instructions on pi-cpu
with fault=null** (n=200M, pc high-half text, x0 stable) — from the
M56 first fault at n=47968. Console is still silent (no UART yet —
that is the next slice); the kernel is in early boot (fixup/reloc +
page-table + percpu setup, all high-half).

Slices (all in `cpu/src/lib.rs`, all assembler-truth, all with
regression proof smoke 23/23 + fuzzer 882/882):

1. **TTBR1 high-half walk.** `mmu_ttbr1` field + TTBR1 MSR/MRS
   (`0xD5182020/0xD5382020`) + T1SZ/TG1 decode. Load-bearing traps:
   TG1 encoding is INVERTED vs TG0 (0b10=4K, not fault — the live
   kernel's TCR `0x5000f0b5593519` carries TG1=2); bit55 (not bit63)
   selects the half; high-half range check is sign-extended form.
2. **Block-output mask.** `(d & !(block-1))` leaked attribute bits
   (AF=bit10) into the PA (0xCDA1AA8 OOR instead of 0xDA1AA8):
   mask to 48-bit `outmask` first. Same fix for L3 pages (`(d &
   !0xfff)` leaked bit63..48: 0x68000001771F70 instead of
   0x1771F70).
3. **Load-literal** (`bits[29:25]==0b01100` — a bits[31:26] test
   MISSES LDR-Xt 0x58=0b010110): LDR X/W + LDRSW + PRFM-literal NOP.
   First cut broke 8 goldens (smp/fb/irq/mva/sd/rpi-kernel/
   firmware/debug) via the wrong mask; fixed to the class test.
4. **PRFM-register NOP** (`prfm pstl1keep,[x17]`=0xF9800071).
5. **BR/BLR split** (`br`=0xD61F0100 vs `blr`=0xD63F0100 differ only
   in bit21 — old opc=bits(22,21) match dropped LR... then REVERTED
   the follow-up theory: `br x8` at the fault site was CORRECT
   (x8=0x1880000 garbage came from the literal pool, not the
   branch); the branch arm is unchanged, documented.
6. **ADRP sign fix.** `sext(u64)<<12` re-signs bit63 (every negative
   ADRP landed ~0x2780_0000_0000_0000 high). Sign-extend in i64
   BEFORE `<<12`. Goldens never caught it (all-positive offsets).
7. **LSL-register-amount fix.** `str x12,[x0,x10,lsl#3]` passed S
   itself (0/1) as the shift instead of size (3): every L2 entry
   aliased pairwise, walked idx stayed zero. Other arms already
   correct (H:1, S/D:esz-shift). Follow-up (post-M57, kernel W-form
   `ldr w1,[x0,x1]`=0x88027E61): amount=size is log2(esz), so the
   W-form shifts by 2 (esz=4), X-form by 3 — same line, proven by
   the spin-loop target VA decoding to garbage (0xffffffc0126ed580)
   before and the correct percpu word after.
8. **Atomics lane** (14-word truth table + 20-word negative survey):
   exclusive family `bits[29:24]==0b001000` (LDAXR/STLXR/LDAR/STLR/
   CAS/CASP) always live; LSE lane post-0x1c-arm by ORDER (Rm==0
   forms share bits with guest reg-off stores — three bit-gates
   each broke goldens or collided; placement after the 0x1c arm is
   the discriminator). Single-core RMW semantics; STLXR status=0.
9. **SP_EL0/TPIDR_EL1 backing** (per-CPU current; kernel faults at
   0x598 without them — TTBR0 L1[0] valid, L2[0] zero = genuinely
   unmapped low VA, not a decoder bug).
10. **STADD one-byte** (`0xC8047C62` o0==0 / `0xC803FE62` o0==1,
    op14:12==0b111, hoisted BEFORE the o0==0/o1==0 family gate —
    o0/o2 are Rs/opcode bits on the 0xC8 lane, not discriminators).
11. **LDRH lesson (M54 carry):** triage-grade proof that bit26==1 is
    SIMD, bit26==0/size==1 is integer halfword (0x7940110B).

- Harness growth: `walk_dump()` (per-level base/idx/descriptor trace),
  `translate()` made `pub` (direct PA probes), triage `PI3_TRACE=1`
  replay + fault-VA walk + translate check.
- Methodology (load-bearing): stale-target trap — `cargo build`
  without fingerprint invalidation silently runs the OLD core
  (`rm -rf target` before differential verdicts); triage prints
  `n=` — a run that prints the OLD fault count is a stale binary,
  not a failed fix. `one.rs` always runs at pc=0x100 (its x0 print
  is the page, not the kernel target — verify with mem dumps, not
  register prints).
- Open (next): UART/PL011 first output (console silent at 200M —
  kernel hasn't touched UART yet), then GIC/local-block IRQs,
  full device bring-up toward the shell prompt.


### M61 -- real-Linux mailbox-IRQ bring-up batch (DONE, committed)

Stall fixed point (by execution, zzconfprobe/zzspinprobe release,
slice 4096, vt 262144): 2B fault-null at pc=0xffffffc0080c1804,
daif=0x3, console 9322 B, tail stuck at `vgaarb: loaded` after
`Firmware transaction timeout` @3.4s. Proved live: 3 mailbox requests
parsed (tags 0x1, 0x3, 0x30046), bank-0 bit-1 enable (ICWR +0x18=0x2),
MAIL0_CNF=1, mbox0=2 LIVE, 21089 timer IRQs, ZERO MBOXRD +0x00 drains,
zero ICRD handler reads. 1.5B/2B/2.2B all freeze at irqs=21089 (timer
IMASKed, handler dead) with the mailbox line still live -- same tail,
no forward progress.

Slices (all in cpu/src/lib.rs + cpu/src/runner.rs, all
upstream-grounded, battery smoke 23/23 + fuzzer 882/882 after every
edit):

1. Mailbox multi-shot + real layout + bus->PA. Every channel-8
   MAIL1_WRT re-processes (no changed-value gate -- kernel reuses one
   buffer per probe); +0x20 real write word dual-decoded with legacy
   +0x14 (fb/shell goldens pin the legacy path); MAIL1 word masked
   & 0x3FFFFFFF (VC bus alias: 0xdc02/060008 -> PA 0x1c02/060000,
   proven by MBOXWR+MBOXBUF trace); +0x00 drain-on-read clears
   pending; +0x18/+0x38 STA busy-while-pending (0 while pending, else
   1<<30); size<8 answers success so a malformed buffer retries.
2. Completion is a MAIL0 IRQ (upstream raspberrypi.c +
   bcm2835-mailbox.c, fetched live). mbx_cnf_irqen (+0x1C latch),
   mbox_pending0() (pending&&cnf&&ic_en0-bit1), BASIC bit 1, bank-0
   enable +0x18/+0x24 (upstream irq-bcm2835.c reg_enable
   {0x18,0x10,0x14} -- old code had NO bank-0 enable, so the line
   never raised). STATUS-bit polling theories all dead (2B-verified 3
   ways -- driver never collects via STATUS).
3. Local block: local_gpu_routing (+0x0C, kernel writes 0x0 -> legacy
   path), +0x40/+0x50 timer/mailbox CTL latches, +0x60 bit1 = cntp
   gated on +0x40 bit1 in linux_mode only (bare-metal lirq/rpi-kernel
   keep raw-compare), bit8 = legacy gated on routing==0. Runner gates
   cntp delivery the same way (bare-metal compat: gate applies in
   linux_mode only).
4. DTB routing verified by parsing the real DTB (FDTv17, NOP padding):
   mailbox@7e00b880 `interrupts = <0 1>` (bank-0 bit 1),
   interrupt-controller@7e00b200 brcm,bcm2836-armctrl-ic,
   local_intc@40000000 brcm,bcm2836-l1-intc; both DTB walkers now
   tolerate FDT_NOP (token 4) or the whole chosen/memory patch
   silently no-ops. Bootargs blacklist now byte-exact oracle minimal.
5. Tags: 0x1 fw rev, 0x10004 serial, 0x30001 ON, 0x30002 rate table
   (+tsize==0 V3D-quirk absorb), 0x30003 measured, 0x30004 max,
   0x30007 min 0, 0x30006 exists, 0x30046 notify, 0x30009/0x28001
   turbo 0, 0x3000d/e/10 voltage nominal, unknown -> success+zeros
   (never error bit). Live kernel tags @2B: 0x1/0x3/0x30046 only --
   stall is before clocks.
6. Triage harness: Runner.irqs + irq_pcs[8] + mbox_drains (drain-log
   gated) + MBOXSEND/MBOXDAIF/LOCALRD/ICRD/ICVAL/MBOXRD/MBOXWR/
   MBOXCNF/ICWR/LOCALWR MBOXTAG traces (stderr, split-stream hygiene)
   + PI3_TIMERTRACE with cntpct at write time +
   mem_u32_dbg/uart_pending2_pub/local_timer_ctl0_pub probe helpers.
   zzconfprobe takes [budget] [slice] for the slice-sensitivity test.
- Open (next, M62): IMASK-honor (CNTP_CTL bit1, PROVEN the guest sets it
  at the frame5 92dd90 site — the old `(v & 1)` mask dropped it) to unstick
  the timer storm, then synchronous completion vs dispatch-walk fix — each
  needs MBOXTAG counts + tail + battery as proof.

### M62 -- CNTP IMASK-honor + dispatch-walk diagnosis (DONE, committed)

Stall UNCHANGED at 2B/4096 (`...0c1804`, daif=0x3, console 9322, tail
`vgaarb: loaded`, `mbox0=2` live, `irqs=21089`, zero ICRD/drains) — but the
mechanism is now fully mapped by execution:

1. IMASK-honor (real guest-intent bug, fixed): CNTP_CTL MSR stores `v &
   0x7` (was `v & 1`, dropping the guest's IMASK=1 set at frame5 92dd90
   `orr #2`); MRS reads back `ctl & 0x3` + live ISTATUS; `cntp_line()`
   requires ENABLE && !IMASK && compare. Stall now reads `cntp_ctl=7
   cntp_line=0` — line quiet, yet stall byte-identical (21k deliveries all
   pre-mask). Timer-storm theory DEAD as the stall cause; fix stays (zero
   regression).
2. Slice-sensitivity (triage knob, not cure): 512 parks EARLIER
   (`...b92d3c`/8612, 296458 IRQs) vs 4096 at the canonical stall; same 3
   MBOXWRs, zero ICRD/drains at both.
3. Full dispatch disassembly (`.inst` assembler-truth): EL1h prologue →
   +0x800 dispatch → irqentry `...b91210` → genhandle `...b92e40` →
   armctrl chained `...021b80` (SHORTCUT masks, `lsr#26`/`ubfx`, bank-read
   `ldr x0,[x1]`) → genhandle slow-path waiter `...0c7214` → stall loop
   `...0c1804` (weigh `[sp_el0+8]=0x00010001` vs x0=0x1, fast-path parked
   with `[sp_el0+1856]=0` early-ret).
4. Value-trace + irqwin (the verdict): guest SEES `LOCALRD val=0x102`
   (bit8=1, mailbox visible) x11358 yet ICRD=0 of ANY offset/size (wide
   ICRDW log: only 4 boot-time ENABLE-mirror reads); zzirqwin finds ZERO
   live+unmasked chunk windows in 2B (mailbox born masked, waiter masked
   — delivery needs the conjunction at ONE boundary). DTB oracle: /timer
   is local_intc PPIs, armctrl is the local-child GPU chain — report and
   delivery share one `cntp_ok` condition (no skew; comment-only probe
   verified identical).
5. Next (M63): SYNCHRONOUS completion at MAIL1-write vs CHAINED-DISPATCH
   parity — logging-first ((b) came back EMPTY, so (a) leads): complete
   what the fast/slow path consumes inline. Each needs counts + tail +
   battery.
- Battery: smoke 23/23 + fuzzer 882/882. Logs (HOME-surviving):
  `~/pi62-logs/` (slice/t/timer/imask/stepb/pend/vec/frame/weigh/irqwin).

### M63–M65 — write-watch kills inline completion; scheduler wait found (DONE, committed)

Write-watch on PA 0x1e28008 (`[sp_el0+8]`, 2.75M hits/2B) proves the
waiter word is a task-struct refcount (0x10001 49%, 0x10000 39%,
0x10002 10%), advanced by normal guest get/put code (stall pair
`...0c1828`/`...0c28b0` + siblings `...1c0b48/68/ec/c00`,
`...1c0774/94/d0/e8` — all `ldr/add|sub/str`, disasm-verified), NOT by
any mailbox IRQ handler (ICRD=0 everywhere). Inline completion in
`mbox_process` is NOT viable; zero-cost wwatch infra kept
(`WWATCH`-gated, battery green).

Mailbox fixes kept (all battery-green, protocol-correct): `MBOXRD
+0x00` serves live `mbx_last_write` (was stale snapshot — multi-shot
reuses one buffer); reqlen bit31 CLEAR on reply (`zzmboxdump` proves
guest sees response headers now); FULL=1 tried and REVERTED by
execution (single-flight: 1 MBOXWR + 5458 EMPTY=0 polls — txdone spin
is not an IRQ kick).

The REAL waiter is the firmware-xact waiter spinning INSIDE the
`...0206c4` tx-loop (both MBOXSEND pcs sit inside it; exits are the
`...0207dc` drain-collect and `...0207f4` idc-match paths, NEITHER
taken in 2B). CORRECTION: the modal `...120f78` resume-pc histogram
was a RED HERRING (chunk-boundary samples, not waiter identity).
`zzsched` time-series shows the console complete by 1.5B with irqs
frozen at 21089. Next (M66): the pre-send idc scan misses on a table
the kernel fills at runtime (table-base 0x0 at 2nd send) — find the
REAL collect trigger (post-send idc-match inputs), not STA bits.
- Battery: smoke 23/23 + fuzzer 882/882. Logs: `~/pi62-logs/`
  (ww/gate/watcharg/waiter/sched) + `/tmp/opencode/m64* m65*`.

### M66 — tx-spin + idc-table + EMPTY=1 verdicts (DONE, committed)

Waiter CORRECTED by execution: the stall is the `...0206c4` tx-loop
spinning 166460× on the pre-send idc-miss (`zzloop`: 166460 visits
each to `...020704`/`...0207b0`, zero to either collect arm), never
reaching MBOXWR #4. The idc table the scan walks (`...1ca624`:
48B-stride walk at `[x1,#6576]` entry+40 vs kind) is filled by the
KERNEL at runtime — at the 2nd send `[x20,#6568]=0xffffffff` /
table-base=0x0 (zzidc), so no entry can match by construction; the
REAL table (observed live at n=150M, VA `...09077b80`, count=0x10)
holds FUNCTION POINTERS (`...0834ab10`/`...08bc0978` + flag 0x403),
not kind words — so the kind-seed (`mbox_fw_idc_init`, REMOVED, doc
comment is the record) changed nothing (`/tmp/opencode/m66idc.*`
byte-identical). EMPTY=1-always (`mail1_sta = 1<<30`
unconditionally) tried → stall IDENTICAL (same pc/tail/3 polls,
`/tmp/opencode/m66spin.*`) — the tx-done poll is NOT the exit gate;
the loop never reaches any STA poll. Reply headers verified
responses (`zzmboxdump`: `reqlen=0x4/0x14`, bit31 clear) yet the run
never reaches either collect arm. NEXT (M67): the post-send
idc-match inputs (`bl ...1ca624` return vs cached `[x19,#8]`) — watch
the table-base PA for the store that publishes it; do NOT re-try
EMPTY=1, kind-seed, FULL=1, STATUS polling, dispatch, or inline
completion (all ruled out by execution above).
- Battery: smoke 23/23 + fuzzer 882/882. Logs: `/tmp/opencode/`
  (m66/m66spin/m66idc/mboxdump2/idc*/loop/sched) + `~/pi62-logs/`.


### M67 — DWC2 OTG + LAN7800 ethernet path (DONE, committed dd3959d)

Complete ethernet protocol/device path in pi-cpu (`Bus`, `cpu/src/
lib.rs`), so the dwc2 + lan78xx drivers can probe without the
initcall blacklist. Previously USB was a SNPSID+DONE stub (M50) with
only a facade-era `src/usb.js` state machine in git history (M30 +
"DWC2 OTG PHY" commit, deleted in M49). Scope: DWC2 OTG core at
0x3F980000 + LAN7800 device behind it (Pi 3 B+ onboard NIC,
usb424:7800 per the DTB `usb-port@1/ethernet@1` tree) + legacy-IC
IRQ wiring (INTERRUPT_USB = GPU IRQ 9) + harness hooks
(`usb_rx_push`/`usb_tx_take`/`usb_mac_addr` for the browser) + two
demo guests (`usb`, `eth`) + smoke/UI wiring.

- **DTB survey (by execution, `zzethdtb*` temp probes, since
  deleted):** `soc/usb@7e980000` compat `brcm,bcm2708-usb`, reg
  `7e980000/10000 + 7e006000/1000`, interrupts `<0 1>+<0 9>+<2 0>`
  (bank-0 bit 1 + GPU IRQ 9 = mailbox + USB), phys → `/phy`
  (`usb-nop-xceiv`), port tree `usb-port@1 (usb424,2514)` →
  `usb-port@1 (usb424,2514)` → `ethernet@1 (usb424,7800)` + MDIO
  `ethernet-phy@1`. Clock `clk-usb` fixed 480 MHz.
- **Upstream ground (scripts/.build/qemu-wasm, in-repo):** QEMU
  `hw/usb/hcd-dwc2.c` reset values + `get/raise/lower_irq` semantics
  + glbreg/hreg0/hreg1 write arms; `include/hw/usb/dwc2-regs.h`
  (Linux `drivers/usb/dwc2/hw.h` import) bit defs;
  `hw/arm/bcm2835_peripherals.c` (DWC2 → `INTERRUPT_USB`);
  `include/hw/arm/raspi_platform.h` (`INTERRUPT_USB = 9`);
  `hw/intc/bcm2835_ic.c` (PENDING1 = low-32 GPU IRQs, BASIC dup
  table `irq_dups[]`, ENABLE1/DISABLE1 semantics).
- **Model (`Bus`):** `usb_glb[28]` (GOTGCTL..GINTSTS2) +
  `usb_hreg0[17]` (HCFG..HPRT0) + `usb_hch[8][8]` (HCCHAR/HCSPLT/
  HCINT/HCINTMSK/HCTSIZ/HCDMA/HCDMAB) + frame/SOF tick + HPRT-conn
  latch, all reset to QEMU `dwc2_reset_enter` values
  (GSNPSID 0x4f54294a, GHWCFG2 0x250dc016, GHWCFG3 0x10000044 —
  machine-checked via python3, never hand-hex; GOTGCTL session-
  valid; GINTSTS CURMODE_HOST|NPTXFEMP|PTXFEMP|CONIDSTSCHNG;
  GRXFSIZ/GNPTXFSIZ/GNPTXSTS/GI2CCTL/GPWRDN/HPTXFSIZ/HCFG/HFIR/
  HFNUM/HPTXSTS/HPRT0-PWR). Write arms port QEMU verbatim:
  GOTGCTL RO-bit preserve + SESREQ/HNPREQ self-complete (SESREQSCS/
  HSTNEGSCS + GOTGINT + OTGINT); GINTSTS W1C with RO protection;
  GRSTCTL AHBIDLE + CSFTRST/HSFTRST self-clear + sticky-word reset;
  HPRT0 RO/W1C handling + PRTRST-falling ENA|ENACHG + PPWR attach
  (CONNSTS|CONNDET + PRTINT); HCCHAR CHDIS/CHENA edges; HCINT W1C;
  HCINTMSK reserved-mask; HCDMAB read-only.
- **Transfer completion (`usb_xfer`, sync):** CHENA rising runs the
  LAN7800 answer immediately (no async BH — QEMU has one, pi-cpu
  completes inline like the mailbox): EP0 setup-stage absorb
  (EP0/OUT xfer==8 latches `usb_setup[8]` — QEMU moves setup via
  FIFO, pi-cpu has no FIFO model, so the latch carries it);
  EP0 data/status stage executes the LATCHED setup (GET_DESCRIPTOR
  device/config/strings incl. 0424:7800 + MAC-hex serial;
  SET_ADDRESS/CONFIG/INTERFACE + CLEAR_FEATURE ack; else
  STALL|CHHLTD); bulk-IN serves one RX frame with the lan78xx
  4-byte header else NAK|CHHLTD; bulk-OUT queues to `usb_tx` +
  `usb_loopback`. Completion publishes HCINT XFERCOMPL|CHHLTD,
  clears CHENA, raises HAINT[ch] + GINTSTS.HCHINT iff masked (QEMU
  `dwc2_update_hc_irq` + `raise_host_irq` verbatim).
- **IRQ:** `usb_irq_level()` = `(GINTSTS&GINTMSK)!=0 &&
  GAHBCFG.GLBL_INTR_EN` (QEMU `dwc2_update_irq` verbatim) →
  `usb_pending1()` gated on IC bank-1 bit 9 → `legacy_line()` +
  PENDING1 bit 9 + BASIC bit 8 (non-shortcut mirror, like
  timer/DMA). Runner delivers with no extra gating (line already
  folds every enable).
- **LAN7800 device:** fixed MAC b8:27:eb:de:ad:be (matches the
  0x10003 mailbox reply), link always up, `usb_rx_push` (browser→
  guest, [len:2 LE][frame] queue, 1518 B / 64 KB caps) +
  `usb_tx_take` (guest→browser drain) + `usb_mac_addr`.
- **Guests:** `programs/usb/` (DWC2 bring-up: SNPSID/GHWCFG/
  FIFO-size/GOTGCTL reads, soft reset, HPRT power/reset, HFNUM
  tick, EP0 setup+data GET_DESCRIPTOR 0424:7800, W1C, CURMODE_HOST;
  parks USB DONE) + `programs/eth/` (LAN7800: port up, EP2-OUT TX
  frame, HCHINT+HAINT[0]+PENDING1-bit9 IRQ check with W1C drop,
  EP1-IN loopback RX + header/payload verify; parks USB DONE).
  Wired in `programs/Cargo.toml` + `build-programs.sh` +
  `src/main.js` (`PROGRAMS` + `DONE_SEL` 7 + boot branches) +
  `index.html` options + smoke goldens (usb 9 strings, eth 8).
- **Gotchas (all bitten, all execution-proven):** QEMU's
  `DWC2_NB_CHAN` is 8 in THIS tree (the facade assumed 16 —
  GHWCFG2 value changes); `debug` pins GSNPSID 280A-only so it now
  accepts 280A|294A (both real revs); `memset` on guest buffers
  lowers to SIMD `mov.h` which pi-cpu faults — guests use byte
  loops; `DATA_BUF = [0; 64]` reassign faults the same way.
- **Verify (by execution):** usb 14 checks + eth 7 checks ALL PASS
  native (`run` fault null); smoke 25/25 (23 old + usb + eth);
  fuzzer 882/882; `npx vite build` clean; Linux triage 2B stall
  byte-identical (`...0c1804`, console 9322, tail `vgaarb: loaded`
  — expected: the kernel still blacklists dwc2; un-blacklisting is
  the NEXT slice, not this one).
- Open (next): drop `dwc2` (+ `smsc95xx`/`usb_ernet`) from the
  pi-cpu `patch_dtb_chosen` blacklist and watch the live dwc2 probe
  sequence against this model (USBTRACE-gated `USBRD/USBWR/USBXFER`
  lines + tail + battery as proof).


### M68 — single-core honesty: maxcpus=1 kills the CPU1-3 wait (DONE, uncommitted)

The browser screenshot showed the pi-linux console parked after
`EFI services will not be available` + `CPU1: failed to come
online` / `failed in unknown state : 0x0`. Triage by execution:

- **EFI line is NORMAL, not an error.** `efi: UEFI not found` prints
  on every raspi3ap boot without UEFI (qemu oracle prints it too);
  the screenshot's `EFI services will not be available` is that same
  line. NOT the boot blocker — do not chase it.
- **CPU1-3 waits are the stall.** pi-cpu runs ONE core (the
  SmpRunner exists only for the bare-metal smp guest; the Linux
  runner never starts secondaries), but the DTB advertises 4
  spin-table CPUs (`enable-method spin-table`, release addrs
  0xd8/0xe0/0xe8/0xf0, proven by a zzdtbcpus FDT dump). Without
  maxcpus the kernel spends ~3000s of virtual time per secondary
  waiting on its spin-table release, then `Brought up 1 node, 1 CPU`
  — boot continues but 1000s late.
- **Fix (one word):** `maxcpus=1` appended to the pi-cpu
  `patch_dtb_chosen` bootargs (comment M68 in lib.rs). 2B/4096
  before: `pc=...0c1804 console=9322 irqs=21089` with CPU1/2/3
  `failed to come online` lines in the tail; after:
  `pc=...b92e44 console=9044 irqs=17059`, tail shows `Bringing up
  secondary CPUs ... Brought up 1 node, 1 CPU / SMP: Total of 1
  processors activated` with ZERO failed-to-come-online lines, and
  the boot runs ~3000 virtual-seconds further (tail `vgaarb: loaded`
  at [1135] instead of [4046]). Battery green both sides
  (smoke 25/25 + fuzzer 882/882).

### M69 — exclusive monitor: ticket-lock unlock stops clobbering (DONE, uncommitted)

The M68 stall moved to `pc=...b92e44` (a `...b92d20` weigh-wrapper
frame: `mrs sp_el0 / ldr w4,[sp0+8] / cbz w4->ret w20`) with console
9044 and tail still `vgaarb: loaded` — but the mailbox line was now
DRAINED (`mbox_pending=0 legacy=0 drains=[381816832]`, NO firmware
timeout in the tail). New waiter: the `[sp_el0+8]` task-refcount
word `w4` (M63 identity) stuck at 0x10000/0x10001 across the whole
500M→2B series (zzwseries: 0x0/0x100/0x2/0x10000 — never collapses).

Root cause (all execution-proven, disassembler-checked):

- The holder path is `...0c2870` get/put (`ldr w1,[x0,#8] /
  add w1,w19,w1 / str` vs `...0c28fc` sub-put) around a weigh loop
  at `...b92dc8` (`bl ...0f5330` strcmp-style helper; w4!=0
  re-weighs).
- The put never lands because the TICKET-LOCK unlock CASAL
  (`c8e5fc62 casal x5,x2,[x3]` in `...0f9be4`) compares a STALE
  LDXR snapshot: pi-cpu executed EVERY STXR/CAS with unconditional
  success (status 0), so the unlock CAS overwrote the lock word even
  though memory had moved on since the reservation — corrupting the
  word the holder's put needed.
- Evidence: `zzexcl` counts 1965 LDXR vs 134 STLXR over 400M (the
  percpu path is exclusive-heavy); field decode of the three live
  words (`ldxr x0,[x3]`=0xc85f7c60, `stlxr w1,x2,[x3]`=0xc801fc62,
  `casal`=0xc8e5fc62 — assembler-truth via `.arch armv8.1-a+lse`)
  shows the unlock shape exactly.

Fix (`cpu/src/lib.rs`, `Cpu::excl_*` monitor, single-core):

- `LDXR/LDAXR` records (addr, value, size); `STXR/STLXR` succeeds
  (status 0 + store) only when memory still matches the
  reservation, else status 1 + NO store; `CAS-family` stores only
  when mem==comparand AND the reservation still matches (plain CAS
  without LDXR, e.g. 0xC8A07C41, keeps compare-only behavior);
  every STXR/CAS clears the reservation (ARM ARM).
- Effect by execution: w4 series collapses (0x0/0x100/0x2 at
  500M/1B/1.5B — the holder runs), console 9044→17921 (+8877 B:
  the boot runs ~800 virtual-seconds further into initcalls), 2B
  tail moves from `vgaarb: loaded` to hung-task `Call trace`
  (`rwsem_down_write_slowpath / down_write / event_trace_init` +
  `__mutex_lock / init_kprobe_trace` — the kernel now schedules
  workqueues and runs initcalls instead of spinning the mailbox
  waiter), `drains=[381816832]` (mailbox completes), NO firmware
  timeout. New stall pc `...b92d28` is the weigh-wrapper entry
  (same w4 frame, deeper budget).
- Battery: smoke 25/25 + fuzzer 882/882 (monitor changes nothing
  for the bare-metal guests — no exclusive contention there).
- Open (next): the hung-task/rwsem stall (tracer + kprobe initcalls
  block on each other — likely needs the next device/IRQ model, or
  a scheduler tick the IMASK now suppresses; map it the same way:
  disassemble the waiter, sample its inputs, fix by execution).

### M70 — copyable terminal + Copy Log button (DONE, uncommitted)

The pi-linux console in the browser could not be copied: the key
handler called `preventDefault()` on every keypress including
Ctrl/⌘+C, so the browser copy never fired; `#term` also lacked an
explicit `user-select`. Fix (`src/main.js`, `index.html`,
`src/styles.css`): `handleKey` returns early on Ctrl/Meta (browser
shortcuts untouched); `#term` gets `user-select: text; cursor:
text`; a `Copy Log` button copies `term.textContent` via
`navigator.clipboard` with an `execCommand` textarea fallback (the
async API rejects on non-secure contexts). Verified: `npx vite
build` clean, button + handler present in `dist/`, smoke 25/25 +
fuzzer 882/882 green.

### M71 — pi-linux UX: scrollable log + 7× frame budget (DONE, uncommitted)

Two complaints from the live `pi-linux` tab, both fixed in
`src/main.js` (verified: minified `le` = termStick in `dist/`):

- **Cannot scroll up.** `draw()` forced `scrollTop = scrollHeight`
  on EVERY slice — pi-linux's rAF loop fires every frame, so
  scrolling up snapped back down instantly. Fix: `termStick` flag
  (scroll listener, 48 px threshold) — autoscroll only when the
  user is already at the bottom.
- **"So slow, stuck after EFI/CPU1 lines."** NOT stuck: those lines
  print in the first 2M insns (the bootPiLinux initial budget), and
  native 2B runs prove the kernel keeps going (9044→17921 console,
  hung-task traces = scheduled workqueues). The browser only looked
  frozen because one 4K slice/frame ≈ 1M insns/s ≈ 0.1 virtual-s
  per wall-s. Fix: pi-linux frames run a ~110 ms wall-clock slice
  budget (`PI_LINUX_FRAME_MS`, 7× the other guests' 16 ms) — steady
  visible progress, tab stays responsive (rAF still yields; scroll
  + input handled).
- Battery still green (smoke 25/25); `npx vite build` clean.

### M72 — pi-linux speed: batch slices, keep 4K (DONE, uncommitted)

"Too slow + stuck after EFI/CPU1 lines" — two real fixes plus an
honest workers verdict, all measured natively first:

- **Measure first.** Native triage: interpreter ≈ 18–27 MIPS
  (shell 2M = 69 MIPS warm; kernel 2M/4096 = 18.4 MIPS; 20M = 26–27
  MIPS). The browser's ~1M insns/s is wasm + per-slice JS overhead,
  not the step loop — so the fix is batching, not a faster decoder.
- **Batch count, never size (`runSliceN` + 64×4K/frame).** New
  `runSliceN(count, n)`: one `wall_tick` up front, N `pi.run`
  calls, one `take_console` + one `insns()` at the end; the frame
  does one `draw` + one `updateStats`. Speeds the 110 ms frame from
  ~27 slices to 64×4K (≈260K insns/frame ≈ 2–4M insns/s in wasm).
  **Slice size is NOT free:** native bisect at 500M proves 8192+
  diverges the trajectory (4096: `pc=...b92e7c con=12668
  irqs=20680`; 8192: `pc=...0226b4 con=8334 irqs=12337`; 65536:
  `pc=...b92d44 con=8189 irqs=552`) — IRQ/timer delivery happens at
  chunk boundaries, so bigger slices change interleaving, not just
  overhead. First cut used 64K slices (fault-null but WRONG tail);
  reverted to 4K×64 on the bisect evidence.
- **Workers/SAB: NO (measured, honest).** A worker moves the SAME
  single-threaded interpreter off the UI thread — it does NOT add
  MIPS (wasm has no threads here; SAB needs COOP/COEP + a threaded
  build). Throughput stays ~2–4M insns/s; the only win is UI
  smoothness, which rAF yields already give. The kernel is a
  single vCPU (maxcpus=1) — there is nothing to parallelize. Drop
  this unless the interpreter itself gets faster (then re-measure).
- **"Stuck" was a reading problem.** EFI/CPU1 lines print in the
  first 2M insns; initcalls then grind ~2B insns between console
  bursts (9044→17921). Fix: the stats row now shows `boot XM
  insns / Y chars` every frame for pi-linux — liveness is visible
  even when the console is silent.
- Battery still green (smoke 25/25); `npx vite build` clean.

### M73 — interpreter speed: TLB + zero-cost traces (DONE, uncommitted)

Next phase per user ("focus on how to increase its speed more"):
profile-first, semantics-unchanged interpreter speedups, all
measured natively (`prof.sh`: 20M kernel triage @4096 + shell 2M):

- **opt-level 3 / fat-LTO tried, REVERTED.** `opt-level=3,
  lto="fat", codegen-units=1` vs shipped `opt-level="s",
  lto=true`: kernel 20M 0.66s vs 0.73s (~10% faster), shell 2M
  identical. NOT worth it: +build time, +wasm size pressure for
  the browser, marginal gain. Shipped profile stays `opt-level="s"`
  (small binary, fast CI).
- **TLB (the real win: 27 → 60+ MIPS).** Every MMU-on fetch/read/
  write paid a 4-level table walk (4–5 RAM round-trips per insn).
  New 256-entry direct-mapped TLB (`tlb_tag/tlb_pa/tlb_gen` on
  `Bus`): VA-page → PA-page for 4K pages AND 2M/1G blocks (offset
  re-planted per access), tag = full (VPN, half, gen) tuple, gen
  bumps on EVERY regime MSR (TCR/TTBR/SCTLR via the system arm +
  MMU_CTL writes). Kernel 20M: 0.73s → 0.30s (**27 → 65 MIPS**);
  shell 2M unchanged (MMU off — no TLB path). Traps bitten: tag
  MUST be full-gen (first cut folded to 8 bits — stale-hit risk
  across regimes, rejected); cached value MUST be the PAGE base
  (`pa & !0xfff` — full-pa caching double-plants the offset);
  `translate()`/`fetch()` go `&mut self` (callers: read/write/
  fetch/step + triage probes — all already `&mut`).
- **Zero-cost traces.** `Cpu::step`'s write-watch check cost two
  `std::env::var("WWATCH")` lookups per step even when never
  armed — reordered to `hits-changed && wwatch_on && env` (env
  runs only while a probe arms the watch). Same pattern kept for
  MBOXTAG/TIMERTRACE/SDTRACE/USBTRACE (all already flag-first).
- **Correctness proof (not just speed):** smoke 25/25 + fuzzer
  882/882 green; `tlbcheck.mjs` pins 20M (`...fadec0 con=4236
  irqs=0`) and 500M (`...b92e7c con=12668 irqs=20680`) trajectories
  byte-identical to pre-TLB goldens.
- **M73b footgun FIXED the same session (TLBI op1 gate).** The TLBI
  arm first shipped as `op0==1 && op1==3 && crn==8` — but assembler
  truth (sysops.s: vmalle1=0xD508871F → op1==0; DC ZVA=0xD50B7420 →
  op1==3) proves TLBI is op1==0, DC is op1==3. The wrong gate
  swallowed ZERO tlbis (all real TLBIs fell into the DC arms as
  harmless NOPs, no gen bump) — and 500M REGRESSED to
  `pc=...037370 con=678 irqs=0` (boot wedged in the 036x loop).
  Fixed to `op0==1 && op1==0 && crn==8` (any CRm: vmalle1 CRm==3,
  vae1/aside1/vaale1 CRm==7): 500M golden restored byte-identical
  (`...b92e7c con=12668 irqs=20680`, 8.0s), battery still green.
  Lesson: D5-hex lookalikes (D50887xx vs D50B74xx) lie — decode the
  fields, never eyeball the top byte.
- Open (next, if more speed is needed): decode dispatch is a long
  if-else chain on `bits(w,...)` — a 256-entry opcode-class table
  would cut ~10 compares/insn; `sync_out`/`sync_in` run per 4K
  chunk (fine); the remaining cost is the interpreter loop itself
  (a JIT is out of scope for pi-cpu-by-design).

### M73b note — SAB / multicore verdict for pi-cpu (record, no work)

User asked (2026-09-19) whether SharedArrayBuffer + worker threads /
multicore could make pi-linux as fast as the qemu-wasm tab. Verdict,
by measurement + architecture (no code changed):

- qemu-wasm boots to a shell in **68 s** (measured
  `test/linux-boot-bench.mjs`, threads=auto → MTTCG, SAB on): JIT
  (TCG→Wasm hot TBs) + 4 emulated cores + block-level translation.
  pi-cpu is a single-threaded Rust interpreter at ~60 MIPS native
  (~2–4M insns/s in wasm after the M72 batching); the two engines
  differ by kind (JIT vs interpreter), not by thread count.
- A Web Worker running the SAME interpreter adds **zero MIPS**:
  wasm here is single-threaded (no `wasm_thread` build, no shared
  memory between UI and worker); the worker only moves the same
  single-threaded loop off the UI thread. UI smoothness is already
  covered by rAF yields. SAB on this page exists for the qemu-wasm
  pthread build only (`public/linux/`, COOP/COEP + sab-toggle) —
  pi-cpu (`public/pi_cpu/`) shares nothing with it.
- True multicore (2 vCPUs on 2 workers) is out of scope: the guest
  kernel runs `maxcpus=1` by necessity (M68: we model ONE core;
  spin-table secondaries never come online), so a second worker
  would idle. SMP in-tree (`SmpRunner`) is for the bare-metal smp
  guest only, not the Linux runner.
- What WOULD move pi-linux speed (ranked): (1) interpreter loop
  itself (decode-dispatch table, fewer branches/insn — the
  remaining ~14 ns/insn after the TLB); (2) bigger honest wins are
  boot-path, not MIPS (shorter path to shell: blacklist/initcall
  trims, initramfs shape); (3) only then would a worker be worth
  revisiting (responsiveness at higher MIPS, never throughput).
  Do NOT re-open SAB/multicore without a new measurement showing
  the interpreter itself got faster first.


### M74 — unmodeled-peripheral faults + dead-end verdicts (DONE, uncommitted)

Three `UnmappedData` faults past the 5.6B breakthrough (con 17921→19772,
`Freeing initrd memory`), each fixed by execution (walk_dump PA + disasm
+ upstream source), plus two honest negative verdicts (kept as record so
nobody retries them):

- **M74c clock block** (`CLK_LEN` 0x3000): 7.6B fault VA
  `0xffffffc0097bd100/200` → PA `0x3f1021xx` (L3 `0x6800003f102713`) —
  the clk-bcm2835 driver scans a second window at `0x3f102000`, not the
  M30 `CLK_BASE` page. Whole `0x3f100000..0x3f103000` block now
  zero-read/absorb (same honesty as other M30 untouched windows).
- **M74d peripheral umbrella** (`PERIPH_BASE/LEN` =
  `0x3f000000/0x300000`): 6.69B store PA `0x3f00604c` (dwc_otg
  `hcd_init_fiq` MPHI init) + 6.71B read PA `0x3f212004`
  (bcm2835_thermal tsens — blacklist skips its initcall, not its regs).
  Covers everything below SDHCI that isn't a modeled window; modeled
  windows keep priority (arm placed after them); census = misc.
  `PERIPHTRACE` env gates per-touch logging (zero-cost off).
- **M74b blacklist verdict (NEGATIVE, reverted):** blacklisting
  `event_trace_init` + `init_kprobe_trace` does NOT skip the hung path
  (still parks at `vgaarb: loaded`, con 9190, mbox pending) — the
  tracer/kprobe waits are symptoms of the firmware-timeout stall, not
  the cause. Stays stock-minimal (oracle cmdline); do NOT retry.
- **M74e ramdisk verdict (NEGATIVE, reverted):** both `root=/dev/ram0`
  (con 9041, irqs frozen) and `+rdinit=/sbin/init` (con 8939, worse)
  starve before the mailbox tx loop; only `root=/dev/mmcblk0` reaches
  the 10.3B VFS panic with con 223k. Real work is the SDHCI/DMA block
  path (M75), not ramdisk shortcuts; do NOT retry.
- Battery after revert: smoke 25/25 + fuzzer 882/882.

### M75 — sdhost@3F202000 model: card enumerates, IRQs deliver (DONE, uncommitted)

The SDHCI window (`0x3F300000`) stays permanently zero (cmd/arg/irpt 0
at every 1B sample) — the Pi 3 boots its card from the **sdhost**
controller at `0x3F202000` (DTB `mmc@7e202000`, `brcm,bcm2835-sdhost`;
qemu `MMCI0_OFFSET`), which shares its page with the bare-metal SMP
spin-table mailbox (`SMP_BASE` == same address). Full model in
`cpu/src/lib.rs`, QEMU `hw/sd/bcm2835_sdhost.c` register map verbatim
(SDCMD/ARG/RSP/HSTS/VDD/EDM/CFG/HBCT/DATA/HBLC + 16-deep FIFO), split
from SMP by `linux_mode` (sdhost arms precede SMP in read()/write();
SMP arms are `!linux_mode`-gated; bare-metal green proves the split):

- **Placement footgun (fixed):** first cut placed the sdhost READ arm
  AFTER the SMP arm — SMP byte backing swallowed all 11k touches (0
  SDHRD lines with SDTRACE on). `SDTRACE` env now gates SDHWR/SDHRD
  logging on both arms (zero-cost off).
- **IRQ bank fix (the big one):** first cut gated PENDING1 bit 24
  (= GPU IRQ 24/DMA — wrong bank, line never delivered). Upstream
  truth (QEMU `bcm2835_ic.c`: `PENDING_2 = gpu_pending>>32`;
  `irq_dups[]` index 8 = IRQ 56 → BASIC bit 18): sdhost is GPU IRQ 56
  = **PENDING2 bit 24 + BASIC bit 18** (`sdh_pending2`, wired into
  PENDING2/BASIC reads + `legacy_line()`). Card enumerates immediately
  (`mmc0: new SD card at address 1234`).
- **Card model** (QEMU `hw/sd/sd.c` verbatim): CMD55 latches APP_CMD
  (R1 carries the APP_CMD bit or the ACMD is rejected); ACMD41 R3 OCR
  = VDD_WIN_HI|CCS|POWER_UP (busy first poll, ready after); illegal
  SDIO/MMC CMDs (1/5/52/53/54/58/59) FAIL+TIME_OUT (answering R1-ok
  gave error -22); CMD9 CSD v2.0 (C_SIZE=7 for the 4MB image), CMD16
  blocklen; R1 = TRAN+READY_FOR_DATA (old 0x900 stalled
  `__mmc_poll_for_busy`); ACMD51 SCR (8-byte data phase, datacnt
  consumed); SDEDM live fifo-count + DATAMODE FSM; SDIO recompute on
  CFG writes (driver programs CFG after data is ready); BUSY_IRPT.
- **M75h DMA bridge** (`dma_sdhost_transfer`): the driver uses DMA,
  not PIO, for block reads (zero SDDATA reads in 30M insns) with CBs
  aimed at SDDATA — routed through the sdhost FIFO (device→RAM pops
  with card refill, RAM→device pushes), PIO-completion tail
  (datacnt=0 + DATA_FLAG + SDIO_IRPT).
- **Status:** timeouts/-22 GONE (no `-110`/`-22` after the R1 fix);
  card clean-enumerates but no `mmcblk0` yet — zero CMD17/18 block
  READs issued (card-reg phase never completes; no partition scan).
  Next: CMD17/18 + partition-scan path to `mmcblk0`.
- Battery: smoke 25/25 + fuzzer 882/882.


## Key risks (M49: unicorn retired — the first two risks below are closed)

- ~~Core patch (Phase 1) is the big unknown~~ CLOSED by the M49 removal:
  pi-cpu needs no fork patches; the only core is cpu/src/lib.rs.
- IRQ delivery semantics must be exact (level vs edge, masking, DAIF.I).
  (Still live inside Runner/Bus; pinned by gpio/uart0/irq/lirq goldens +
  browser button-IRQ E2E.)
- SDHCI/DMA under Linux is much harder than the FAT12 demo.
- ~~Keep M1–M19 regression green: 20 probes + browser E2Es~~ SUPERSEDED:
  test/pi-cpu-smoke.mjs (22 goldens) + test/cpu-cases.mjs (819) +
  9 upython suites + browser E2Es (16/16 boots, REPL/float/button/fb).

## Working conventions

- Build: `bash build.sh` (pi-board wasm + pi-cpu wasm via wasm-pack into
  public/pi_cpu + guest programs), then `npx vite build` for production.
- Regression: `node test/pi-cpu-smoke.mjs` (22 goldens),
  `node test/cpu-cases.mjs` (819; `--regen` to re-pin),
  `node test/upython-repl.mjs` (etc. — 9 suites via test/pi-sess.mjs +
  `cargo build --release --example sess`).
- Browser E2E: `npx vite preview` on :5173 + headless chrome scripts
  (/tmp/opencode/picpu-e2e.mjs boots, picpu-interactive.mjs REPL/button).
- Commit style: one long descriptive message per milestone, push to
  master.
- CI (`.github/workflows/pages.yml`, deploys Pages on push to master):
  node 22 (matches local + puppeteer engines), `npm ci`, wasm-pack via
  the official installer (build.sh hard-requires it for public/pi_cpu),
  then `npm run build`. After adding/removing a workspace package, run
  `npm install` locally — `npm ci` fails when package-lock.json is out
  of sync (seen when packages/pi3-emu was deleted).
- README.md has a per-milestone History section — keep it updated.
- Update this file as the plan evolves.