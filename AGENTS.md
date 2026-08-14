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
  pwm, i2c, spi, uart1, sd, uart0 (17 programs).
- Scheduler: run-until-idle, 512-instruction slices (`runSlice` in
  src/main.js), devices synced before/after each slice.
- IRQ delivery is host-assisted: slice-boundary delivery, `IRQ_RET` magic
  at IC_BASE+0x2C, vector glue that saves the full register file.

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
  3=uc_gt_irq[0], 4=uc_cntpct, 5=env.pc, 6/7=delivery counters,
  8/9/10=last-TB ring {addr,count,reset}).
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

### Phase 2a — BCM2836 local interrupt block (DONE, uncommitted)

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
- COMMIT CHECKLIST (pending): programs/lirq/, src/localint.js,
  src/main.js, index.html, test/lirq-probe.mjs, public/programs/
  lirq.elf + irqcore.elf, build-programs.sh, programs/Cargo.toml/
  Cargo.lock; README History M20 entry written; push.

### Phase 2 — real devices

- [x] BCM2836 local interrupt block at 0x40000000 (Phase 2a above).
- Rework existing models from window semantics to true MMIO IRQ
  semantics (PL011 RXIM/TXIM, system timer C1/C3, GPIO, SDHCI).
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
- Keep M1–M19 regression green: 18 probes + browser E2Es must not break.

## Working conventions

- Build: `bash build.sh` (cargo board wasm + guest programs + copies
  unicorn.js), then `npx vite build` for production.
- Regression: `for p in branch csel clock dma fb gpio i2c instr irq mbox
  mmu pwm sd smp spi stats uart0 uart1; do node test/$p-probe.mjs; done`
  (all must PASS).
- Browser E2E: vite on :5173 + headless chrome CDP (e.g. :9353), scripts
  in /tmp/opencode/*-e2e.mjs.
- Commit style: one long descriptive message per milestone, push to
  master.
- README.md has a per-milestone History section — keep it updated.
- Update this file as the plan evolves.