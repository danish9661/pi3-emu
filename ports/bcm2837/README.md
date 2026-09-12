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

Image cap: 32 sectors (model `writeSector`); raising it needs the Python
`_MAX_SECTOR` bumped in lockstep. VFS `rename`/`remove`/`rmdir` have no
probe coverage yet. The vendor core's dead arch-timer path is unroot-caused
(irrelevant while nothing but `lirq` uses it — and `lirq` runs on stock).

## time + machine.Timer (done)

`time` (extmod `modtime.c`) runs on the HAL in `uart.c` (`ticks_ms/us/cpu`,
`delay_ms/us`, all off the 1 MHz `TMR_CLO`); frozen `utime.py`
(`from time import *`) keeps Pico code working — this upstream renamed
utime to time. `machine.Timer(id, mode, period, callback)` covers ids
0..3 ↔ system-timer C0..C3, `PERIODIC`/`ONE_SHOT`, `init`/`deinit`;
period is milliseconds. Delivery reuses the `Pin.irq` machinery (legacy-IC
IRQ 1 → local block → vector re-arms PERIODIC + queues → `irq_drain`
runs callbacks main-loop-style). `test/upython-timer.mjs` 11/11.
Load-bearing details: IC enable bit is per-channel (bit `ch`); the CS
model hook is inverted vs hardware W1C (keep-mask), so the driver acks
`0xF^(1<<i)` complements; a disarmed channel's stale compare still fires
once and must be acked or it livelocks; `list.__setitem__` doesn't exist
at this ROM level (callbacks need `def`).

## Build staleness rules (proven by failure, M39)

- The port's custom `upy_%.o` rules now emit `.P` depfiles like upstream's
  `compile_c` — without them, extmod objects never rebuild on
  header/qstr-pool changes and bake in stale QSTR numbers (M39's new
  QSTRs renumbered the pool; stale `upy_modos.o` silently lost
  `os.mount`/`VfsFat` while `sep`/`remove` survived via earlier first-seen
  IDs). Recovery: `rm build/upy_*.o build/extmod_machine_mem.o` + rebuild.
- `CFLAGS` edits never trigger rebuilds (make can't see flags) — `touch`
  the affected sources after changing flags (burned us on `FFCONF_H`).

## NEON-free constraint (load-bearing for the browser core)

`public/unicorn.js` (single-arch) faults on NEON/SIMD but runs scalar VFP
fine — so the firmware must contain zero vector instructions, or browser
upython dies (proven: newlib `strlen`'s `shrn` killed the version banner).
`string_port.c` overrides the 7 linked newlib string/mem functions with
plain C; `-fno-tree-vectorize/slp` keeps GCC from emitting SIMD;
`-fno-builtin` file-local keeps the loops from becoming memcpy calls.
After any toolchain/flag/newlib change, re-verify: `nm` the string syms
to `string_port.o`, `objdump` them for zero vector insns, and run the
battery with the stock core (swap it over the vendor file temporarily —
all 8 suites must PASS).

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

## FAT12 over SDHCI (done, pure Python + real VfsFat mount)

`sdcard.py` (frozen) implements the SD init sequence + CMD17/CMD24 PIO
reads/writes + a minimal FAT12 parser (`ls()`, `read(name)`,
`write(name, data)`) against the 5-sector card; `boot.py` auto-runs it
at startup (`sd: ['HELLO.TXT']` in the banner). `test/upython-sd.mjs`
6/6 incl. the exact HELLO.TXT payload. The card image is real FAT12 now
(boot signature, `FAT12   ` type string, cluster at dir entry +26, size
u32 at +28 — `sdcard.py` keeps legacy fallbacks on read), so the real
kernel VFS mounts it too:

```
import os, sdcard
os.mount(os.VfsFat(sdcard.SDCard()), "/sd")
os.listdir("/sd")            # ['HELLO.TXT']
open("/sd/HELLO.TXT").read() # b'hello from the SD card\r\n'
```

`test/upython-vfs.mjs` 14/14 (mount, listdir, read, seek/tell, mkdir/
chdir/stat, create+write, append, 3.2 KB multi-cluster file, package
import, `import greet.py` from `/sd`, clean post-umount errors, raw/VFS
coherence). `test/upython-vfspersist.mjs` 7/7 (see below). This upstream
calls the module `os` (no `uos` alias).

Auto-mount: `boot.py` mounts `/sd` (and appends it to `sys.path`) when
the host's `SD_PRESENT` word (mailbox window `+0xFF0`, driven every slice
— always 1 in the browser host, 1 iff `attachSdhci()` in the facade) is
set. Reading the flag can never abort, so detached boots stay clean.
Raw `sdcard.write()` also allocates now (free-cluster scan, chain
extend/shrink, both FAT copies) — `test/upython-sd.mjs` 8/8.

One-writer rule: raw writes bypass a live VfsFat mount's sector cache.
Remount before reading back through `/sd`, and never interleave raw
writes with VFS *writes* while mounted — FatFs writes sectors back from
its stale cache and silently clobbers raw-written entries (proven by a
failing persist run; the suite encodes the discipline).

Persistence: `Pi3Emulator.exportCard()` returns the flat sector image
(`sdhci.js` `exportImage`); `importCard(bytes)` restores it (before boot,
or remount after). Small ints are 63-bit here, but `os.stat` timestamps
go through `mp_obj_new_int_from_ll`, which *unconditionally* raises under
the default NONE longint impl — hence `MICROPY_LONGINT_IMPL_MPZ` (LONGLONG
is 32-bit-only upstream and won't compile). The card image carries a
valid 2026-09-10 dir stamp: FatFs date math underflows on zero dates.
