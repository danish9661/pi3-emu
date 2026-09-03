#!/usr/bin/env python3
"""Apply the single-thread (no-SAB) patches to the qemu-wasm clone.

ST-only: applied by scripts/build-linux.sh when THREADS=st, after the N4
patches. Run from the qemu-wasm source root.

Background: ktock's TCG->Wasm JIT backend installs its per-thread
Module.__wasm32_tb exactly once, in mttcg_cpu_thread_fn
(accel/tcg/tcg-accel-ops-mttcg.c). On the thread=single (RR) path that
never runs, so the first hot TB calls instantiate_wasm() with __wasm32_tb
undefined -> TypeError. These patches mirror the MTTCG init onto the RR
vCPU thread so single-threaded execution initializes the backend.

Idempotent: the full `new` block is checked before the anchor (several
anchors are substrings of their own replacement).
"""
import os
import sys

ROOT = os.getcwd()
RR = os.path.join(ROOT, "accel/tcg/tcg-accel-ops-rr.c")


def patch(path, old, new):
    with open(path, "r") as f:
        src = f.read()
    if new in src:
        print(f"already patched (skipped) {path}")
    elif old in src:
        if src.count(old) != 1:
            raise SystemExit(f"ANCHOR AMBIGUOUS ({src.count(old)}) in {path}")
        src = src.replace(old, new)
        with open(path, "w") as f:
            f.write(src)
        print(f"patched {path}")
    else:
        raise SystemExit(f"ANCHOR NOT FOUND in {path}:\n{old}")


# 1) RR file: include the wasm32 backend header (same guard as MTTCG).
patch(
    RR,
    '#include "tcg-accel-ops-rr.h"',
    '#include "tcg-accel-ops-rr.h"\n'
    '#if defined(EMSCRIPTEN) && !defined(CONFIG_TCG_INTERPRETER)\n'
    '#include "../../tcg/wasm32.h"\n'
    '#endif',
)

# 2) RR vCPU thread: init the wasm32 backend (mirrors mttcg_cpu_thread_fn).
patch(
    RR,
    """static void *rr_cpu_thread_fn(void *arg)
{
    Notifier force_rcu;
    CPUState *cpu = arg;

    assert(tcg_enabled());""",
    """static void *rr_cpu_thread_fn(void *arg)
{
    Notifier force_rcu;
    CPUState *cpu = arg;

#if defined(EMSCRIPTEN) && !defined(CONFIG_TCG_INTERPRETER)
    init_wasm32();
#endif

    assert(tcg_enabled());""",
)

print("ST patches applied OK")
