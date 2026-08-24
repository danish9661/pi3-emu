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
(`-accel tcg,tb-size=500,thread=multi -smp 4,sockets=4`) — this is the
working config for ktock's pthread build; single-thread triggers a
`start is not a function` pthread-worker race. The Linux engine runs inside
a same-origin `<iframe>` in the pi3-emu UI; for that iframe to be
cross-origin isolated (SharedArrayBuffer / pthreads), the WHOLE app must be
isolated, so `vite.config.js` now sets `Cross-Origin-Opener-Policy:
same-origin` + `Cross-Origin-Embedder-Policy: require-corp` on every
dev/preview response (the `coi-serviceworker.js` in `public/linux/` is then
a no-op fallback for static hosts that don't send these headers). Verified
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