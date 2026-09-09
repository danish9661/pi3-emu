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

Full VFS mount (blocked: upstream master's VFS state is inconsistent, so
the port ships the pure-Python FAT12 reader below instead), `Pin.irq()`
edge-level triggers beyond RISING/FALLING.

## `machine` module — UART (done)

`machine_uart.c` implements `machine.UART` with Pico-compatible
construction and methods (`read`/`readinto`/`write`/`any`) on PL011 id 0
(full duplex, real IBRD/FBRD divider math for the 3 MHz UARTCLK) and the
mini UART id 1 (TX-only, matching the device model). Notes: consecutive DR
reads in one slice see the same preloaded cell, so each received byte
settles across slice boundaries; `timeout=0` returns available bytes or
`None`. `test/upython-uart.mjs` 7/7 (TX echo, idle `any()`, silent empty
read, `[u1]`-tagged mini-UART output).

## Doubles (floats work natively)

Guest VFP executes fine on the core — no soft-float workaround needed
(AArch64 GCC rejects `-msoft-float` anyway). `mpconfigport.h` enables
`BUILTINS_FLOAT`/`FLOAT_IMPL_DOUBLE`/`MATH` (+`-lm`); `1.5 + 2.25` →
`3.75`, `math.sqrt(2)` → `1.4142135623730951` (covered in
`test/upython-repl.mjs`).

## `machine` module — I2C/SPI (done)

`machine_i2c.c` / `machine_spi.c` implement `machine.I2C` and `machine.SPI`
with Pico-compatible constructors and methods (`readfrom`/`writeto`/
`readfrom_mem`/`scan`, `write`/`read`/`write_readinto`), driven straight
off the BSC0/1 and SPI0 registers against the built-in slaves
(`test/upython-i2cspi.mjs` 9/9: WHO_AM_I/TEMP/COUNTER, JEDEC
`[0, 0xEF, 0x40, 0x18]`, scan, ACKs). Two notes for driver authors: the
high-level API is implemented locally (extmod's shared dicts misbehave on
this minimal config — wrong methods resolve), and every transfer needs a
drop-sync on the DONE cell first (status bits are slice-boundary-visible,
so a leftover DONE completes the next poll instantly — the same
stale-window race the bare-metal guests handle). Transfers cap at 4 bytes
(the FIFO window width); only BSC0/1 and SPI0 exist here.

## `machine` module — Pin (done)

`machine.c` implements `machine.Pin` with Pico-compatible semantics
(`Pin(id, mode, pull)`, `value()/on()/off()/init()`, `IN=0/OUT=1`,
`PULL_UP=1/PULL_DOWN=2`) on the real register file (GPFSEL/GPSET/GPCLR/
GPLEV + the GPPUD pull sequence). Verified against live registers
(`test/upython-machine.mjs` 9/9): FSEL latches, `on()` drives GPLEV21
(the browser LED dot), `value()` reads back, BTN 29 reads press/release.
Note for test authors: GPLEV mirrors the latch at slice boundaries —
settle a couple of slices before asserting levels.

## `machine` module — Pin.irq (done)

`Pin.irq(handler, trigger)` with Pico trigger values, backed by a real
vector table (`vectors.s`, full register save), a C dispatcher that acks
GPEDS W1C and queues callbacks, and deferred dispatch in the stdin wait
loop (main-loop context, ESP32-style). Firmware also enables the GPIO
bank lines (IRQ 81/82) and installs VBAR + `daifclr`. Edge events are
qualified by the live pin level (the model raises on any covered level
change). Verified async — handler fires with no keys typed.

## FAT12 over SDHCI (done, pure Python)

No kernel VFS: upstream master's `mp_state_vm_t` lost its VFS fields, so
`MICROPY_VFS=1` doesn't compile there. Instead `sdcard.py` (frozen)
implements the SD init sequence + CMD17 PIO reads + a minimal FAT12
parser (`ls()`, `read(name)`) against the 5-sector card; `boot.py`
auto-runs it at startup (`sd: ['HELLO.TXT']` in the banner).
`test/upython-sd.mjs` 4/4 incl. the exact HELLO.TXT payload. Two model
conventions honored (same as the sd guest): start cluster at dir entry
+20 (not FAT-standard +26), file size split with length at +30/+31.
