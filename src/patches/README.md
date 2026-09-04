# unicorn.js fork patches (reconstructed)

The `public/unicorn.js` CPU core is a patched + rebuilt
`github.com/AlexAltea/unicorn.js` @ `8028ec43` (engine submodule: unicorn
2.1.4-era QEMU). The original patch set lived only in ephemeral `/tmp` and
was wiped twice — these files reconstruct it from the AGENTS.md record,
re-applied to a fresh clone and verified here (see "Verification" below).

## Source layout

- Top repo (`AlexAltea/unicorn.js`): `build.py`, `src/unicorn-wrapper.js`.
- Engine (`unicorn/`, git submodule): everything under `unicorn/`.

## Apply order (paths relative to the indicated repo root)

Top repo (`unicornjs-src/`):

1. `01-build-exports-wrapper.patch` — export the four new wasm APIs and bind
   them as `Unicorn` methods (`arm64_debug` returns BigInt, `arm64_timer_tick`
   takes a BigInt cntpct).

Engine (`unicornjs-src/unicorn/`):

2. `02-cpu-h-uc-fields-aa64.patch` — `ARMCPU` gains `uc_cntpct`, `uc_ext_irq`,
   `uc_gt_irq[4]` (1:1 onto timeridx: 0 CNTPNS, 1 CNTV, 2 CNTHP, 3 CNTPS),
   `uc_deliver_exceptions`; declares `arm_cpu_update_uc_irq()`; forces
   `arm_el_is_aa64() == true` for EL≤2 (bare-metal reset leaves
   SCR_EL3.RW/HCR_EL2.RW == 0, which otherwise walks EL1 as AArch32).
3. `03-arm-timer-irq-dointerrupt.patch` — generic-timer counter reads
   `uc_cntpct` (host-ticked); recalc/ctl-write/reset bodies drive IRQ lines
   through `arm_cpu_update_uc_irq()` (no QEMUTimers/ptimers in this build);
   `cpu_aarch64_init` sets CNTFRQ 19.2 MHz + clears lines + enables delivery;
   `arm_cpu_do_interrupt` forced onto the A64 entry path.
4. `04-uc-api-plumbing.patch` — dispatch typedefs/pointers (`uc_priv.h`),
   public decls (`unicorn.h`), entry points (`uc.c`), arch implementations
   (`unicorn_aarch64.c`): `uc_arm64_set_irq` (level IRQ),
   `uc_arm64_timer_tick` (advance counter, re-evaluate compares),
   `uc_arm64_debug` (0 interrupt_request, 1 daif, 2 ext line, 3/11/12/13
   timer lines, 4 counter, 5 pc, 6 exception_index, 7 ESR_EL1, 70 FAR_EL1,
   71 SCTLR_EL1, 72 HCR_EL2, else 0), `uc_arm64_deliver_exceptions`
   (delivery gate, default on).

## Deliberately NOT included

- Per-walk/fill/lpae diagnostic rings (uc_arm64_debug sels 8-10/14-111):
  they recorded every page walk and cost ~3000× throughput (0.003 MIPS).
  Rebuilt core returns 0 for those selectors (~10 MIPS).
- `g_assert`/`abort()`/`tcg_abort()` suppressions tried during the Linux
  boot investigation: none of them fixed the abort (dead ends), and they
  would mask real translation errors. The TCI vector-op gap that blocks a
  Linux boot on this core is architectural, not assert-related.

## Rebuild recipe

```sh
source ~/emsdk/emsdk_env.sh   # emscripten 6.0.6
git clone --recurse-submodules https://github.com/AlexAltea/unicorn.js unicornjs-src
cd unicornjs-src
git apply <repo>/src/patches/01-build-exports-wrapper.patch
git -C unicorn apply <repo>/src/patches/02-cpu-h-uc-fields-aa64.patch \
  <repo>/src/patches/03-arm-timer-irq-dointerrupt.patch \
  <repo>/src/patches/04-uc-api-plumbing.patch
python3 build.py aarch64   # -> dist/unicorn_aarch64.js (+ wasm)
```

`bash build.sh` copies the result into `public/`.

## Verification

- All four patches `git apply --check` clean against a pristine clone
  (round-tripped via stash).
- Full `python3 build.py aarch64` (emsdk 6.0.6) succeeds; the result was
  functionally verified: mva guest completes (alias reads OK; only the
  by-design walk-diagnostic assertion differs), lirq 14/14, and the full
  M1–M19 probe battery green. Two fixes found while rebuilding are baked
  into the patches: void C entry points must not use the value-returning
  `UC_INIT` macro, and the arch implementations carry distinct `arm64_*`
  names (public `uc_arm64_*` live in `uc.c`); the wrapper coerces the timer
  argument with `BigInt()` so Number and BigInt callers both work.
