# MicroPython port for BCM2837 (bare metal, AArch64)

Runs the MicroPython VM as a guest program in the pi3-emu emulator: boots to
a `>>>` REPL over the PL011 in milliseconds, no OS, no Linux. Status: spike
green (REPL, arithmetic, variables, heap strings/lists, frozen `import boot`).

## Build

```sh
# one-time: ARM bare-metal toolchain (~130 MB, outside the repo)
# https://developer.arm.com/.../arm-gnu-toolchain-13.2.rel1-x86_64-aarch64-none-elf.tar.xz
export PATH="$HOME/toolchains/arm-gnu-toolchain-13.2.Rel1-x86_64-aarch64-none-elf/bin:$PATH"
make   # -> build/firmware.elf (AArch64, entry 0x100000, ~240 KB)
```

MicroPython source comes from the `ports/micropython` submodule (upstream
master, shallow). `make -C ../micropython/mpy-cross` builds the host
cross-compiler used for frozen modules.

## Run

```sh
node test/upython-repl.mjs   # repo root: boots firmware, checks REPL
```

Or load `build/firmware.elf` in the browser like any guest (UART0 console).

## Layout

- `mpconfigport.h` — minimal ROM level, no floats (integer-only Python,
  no FPU bring-up), GC on, compiler+REPL on, frozen-mpy on, sys module on
  for `sys.path` only (see below).
- `mphalport.h` / `uart.c` — PL011 driver (115200 8N1, same init as the
  uart0 guest), microsecond timer, stdin/stdout, delay.
- `main.c` — 256 KB static GC heap, `mp_init`, friendly REPL, stubs.
- `startup.s` / `bcm2837.ld` — `_start` sets SP=0x3FFFFF0, zeroes `.bss`,
  calls `main` (mirrors `programs/runtime` guests).
- `boot.py` — frozen demo module (`import boot` → `boot.hello()`).

## Lessons (each cost a debug session)

1. **The frozen qstr pool must be real.** A hand-made empty
   `mp_qstr_frozen_const_pool` silently corrupts runtime qstr interning
   (stores land under id 0, later loads miss, names print empty). Always
   generate it with mpy-cross + mpy-tool, even for one tiny module.
2. **Frozen imports need `sys.path`.** `mp_find_frozen_module` is only
   consulted for `.frozen/`-prefixed paths, and that prefix only arrives
   via the `".frozen"` sys.path entry — which needs `MICROPY_PY_SYS`
   (not just `MICROPY_MODULE_FROZEN_MPY`). Keep the port's
   `mp_import_stat` a plain NO_EXIST stub; claiming frozen matches there
   returns prefix-less paths that `do_load` cannot load.
3. **Keep mpy-tool's `boot.py` entry name.** `import boot` matches because
   the import machinery appends `.py` before the frozen search. Renaming
   the table entry to `boot` breaks it.
4. **Drip-feed scripted input.** The PL011 RX FIFO holds 16 bytes; pasting
   long lines at once truncates them (the guest then waits on `...`).
   `test/upython-repl.mjs` paces bytes; humans type slower than the FIFO.

## Next (not yet)

`machine` module (Pin/I2C/SPI/UART on the existing device models),
floating point, FAT filesystem over SDHCI, frozen auto-run `boot.py`.
