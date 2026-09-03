# AGENTS.md — pi3-emu: Raspberry Pi 3 emulator in the browser

## Project overview

A from-scratch Raspberry Pi 3 emulator that runs entirely in the browser:
QEMU TCG compiled to wasm (`public/unicorn.js`, AArch64) as the CPU core,
a Rust → wasm board model (`public/pi_board.wasm`), and JS device models
with real BCM2837 register layouts. Guests are custom bare-metal Rust
programs cross-compiled to AArch64 ELF and loaded into guest RAM.

Repo: `github.com/danish9661/pi3-emu` (master branch, one commit per
milestone M1…M19, long descriptive commit messages).

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

## Verified core facts (unicorn.js 2.2.0 build in public/)

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

## Key risks

- Core patch (Phase 1) is the big unknown: if the unicorn.js build can't
  be patched/rebuilt with exception injection + timer sysregs, the whole
  plan changes (fallback: evaluate qemu-wasm embed as a second mode).
- IRQ delivery semantics must be exact (level vs edge, masking, DAIF.I).
- SDHCI/DMA under Linux is much harder than the FAT12 demo.
- Keep M1–M19 regression green: 20 probes + browser E2Es must not break.

## Working conventions

- Build: `bash build.sh` (cargo board wasm + guest programs + copies
  unicorn.js), then `npx vite build` for production.
- Regression: `for p in branch csel clock dma fb gpio i2c instr irq mbox
  mmu pwm sd smp spi stats uart0 uart1 lirq mva; do node test/$p-probe.mjs; done`
  (all must PASS).
- Browser E2E: vite on :5173 + headless chrome CDP (e.g. :9334), scripts
  in /tmp/opencode/*-e2e.mjs (phase2b-e2e.mjs covers irq/uart0/lirq/gpio).
- Commit style: one long descriptive message per milestone, push to
  master.
- README.md has a per-milestone History section — keep it updated.
- Update this file as the plan evolves.