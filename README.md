# RPi 3 (BCM2837) emulator spike

A Raspberry Pi 3 emulator that runs entirely in the browser — an AArch64
CPU core (unicorn.js), a minimal board model, and a **real ELF loader**:
guest programs are cross-compiled Rust binaries (`aarch64-unknown-none`)
that boot, poll the UART, echo keys, and answer commands — exactly like a
bare-metal OS would.

```
+--------------------------------------------------------------+
| Browser (page)                                               |
|   term / prog select <-> host loop (src/main.js)             |
|     loads the ELF, delivers keystrokes, drains TX, draws     |
+---------------------------+----------------------------------+
| ELF loader (src/elf.js)   | parses PT_LOAD segments, zeros   |
|                           | .bss, sets PC=e_entry, SP=RAM top |
+---------------------------+----------------------------------+
| Board (Rust -> wasm)      |  board/src/lib.rs                |
|   UART console FIFO       |  UART window @ 0x3F201000        |
+---------------------------+----------------------------------+
| CPU: unicorn.js (QEMU TCG AArch64 core, wasm)                |
+--------------------------------------------------------------+
```

The guest program is a live process: it runs in bounded slices from its
current PC; the host stops a slice when the guest goes quiet (i.e. it is
parked polling for input again). Echo, line buffering, command dispatch,
responses and the prompt all run inside the guest — the host only moves
bytes and counts instructions.

## Program loading

The page fetches `public/programs/<name>.elf`, maps RAM + the UART
window with unicorn, and writes each `PT_LOAD` segment at its virtual
address (zeroing the `.bss` gap). The CPU starts at `e_entry` (passed to
`emu_start`) and the guest's own `_start` sets its stack pointer — this
build of unicorn.js ignores host `reg_write` calls, so the guest is
fully self-starting. Any stripped, statically-linked AArch64 ELF linked
at `0x100000` (the runtime linker script) can run.

Programs are built out of `programs/` with plain `cargo build --release`
(same toolchain as the board wasm target, via `.cargo/config.toml`):

```sh
bash build-programs.sh   # -> public/programs/{shell,sum,fib}.elf
```

## Device window (0x3F201000, 4 KiB)

- `+0x00` — TX slot: guest `putc` writes a char then **spins until the
  host clears it** (pulse protocol, exactly one char per slice)
- `+0x80` — RX slot: host writes a byte, guest `getc` consumes it

## SMP (4 cores)

`smp` runs four AArch64 cores (four unicorn instances) on one guest
ELF. Each core has its own private RAM at the same addresses — the
"partitioned DDR" model — and its own stack top (set by a per-core
`_start`). The only shared state is the **mailbox MMIO window at
0x3F202000**, arbitrated by the host between slices, exactly like a bus
master serializing device traffic:

- `+0x00` START_ENTRY[3] — core 0 writes entry addresses for cores 1..3
- `+0x10` GO — core 0 releases the secondaries
- `+0x14` COUNTER — shared counter, device-serialized increment
- `+0x18` LOCK — spinlock flag
- `+0x1C` MSG[4] — per-core result mailbox
- `+0x30` CURRENT — running core id (host-written, observability)
- `+0x34` PARK_MASK — core i sets bit i when done
- `+0x38` CPUID — host-written core id (device-provided identity)

The host scheduler round-robins 512-instruction slices over the running
cores until every core has parked (written its PARK_MASK bit) and the
console is drained. The demo: core 0 launches cores 1..3 through
START_ENTRY, each core computes its quarter of the sum 1..100, reports
through the spinlocked UART, posts its result to MSG, increments the
device counter, and parks; core 0 tallies the mailbox and prints the
final counter.

## System timer (0x3F003000, real BCM2837 layout)

A free-running 40-bit counter ticking in microseconds of host wall clock
(the epoch resets when a program boots, so CLO starts near 0):

- `+0x00` CS — match flags M0..M3 (host sets each bit once when the
  counter first crosses the compare; guest clears by rewriting the mask —
  host-arbitrated memory can only observe byte changes, so CS is
  write-mask here rather than the real chip's W1C)
- `+0x04` CLO / `+0x08` CHI — counter low/high (host-refreshed each slice)
- `+0x0C..0x18` C0..C3 — compare registers (guest)
- `+0x20` DONE — host extension: the clock guest parks by writing 1

The `clock` program sleeps a real wall-clock second by spinning on CLO,
arms C1 for +0.5 s and polls the M1 match bit, then clears it. The shell
has a `time` command printing the live counter.

## VideoCore mailbox (0x3F00B880)

A property-tags request/response mailbox, as used by Linux to ask the
firmware (VideoCore) for board identity:

- `+0x14` MAIL1_WRITE — guest writes `buffer_addr | 8` (channel 8 =
  property tags); the host latches it at the next slice boundary
- `+0x18` MAIL1_STATUS / `+0x04` STATUS — full bit `0x80000000`:
  set when a request is pending, cleared once the host has answered
- `+0x00` MAIL0_READ — returns the request address (response address)

The guest builds a tag list in RAM (`size`, `code`, then
`id|size|code|value` triplets, `0` terminator) and spins on STATUS until
the host arbitrates. The host answers known tags — firmware revision,
board revision, board serial, ARM memory layout, MAC, power state, ARM
clock rate — writing success codes and little-endian values back into
the guest's buffer (multi-byte values are serialized byte-by-byte, so
`0x400000` survives as four bytes, not a clamped `0`). The shell's
`mbox` command queries the board:

```
> mbox
board rev:  a02082
serial:     deadbeef00000000
arm memory: 0x0 + 0x400000
arm clock:  700000000 Hz
```

The request buffer lives in its own linker section (`.mbox` at
0x300000) — the guest `.bss` had grown into the TCG env-hazard zone and
the mailbox round-trip was corrupted intermittently.

## GPIO (0x3F200000, real BCM2837 layout)

Output pins are host-arbitrated like the timer: the host pulls `GPSET0`/
`GPCLR0` out of the window after each slice (write-1 latches) and
refreshes `GPLEV0` before each slice so input pins reflect the UI. The
`gpio` program configures pins 21..28 as outputs and pin 29 as an input
with a proper `GPPUD`/`GPPUDCLK0` pull-down sequence, runs a
knight-rider chase across the 8 LEDs timed by the system timer, then
polls BTN 29 and reports each press edge:

```
> gpio
gpio: LEDs 21..28 output, BTN 29 input (pull-down)
chase:
...
chase done - hold BTN 29
button: 1 pressed
```

The page shows 8 live LED dots and a hold-button next to the terminal;
the chase is paced with `requestAnimationFrame` (slices run for ~16 ms
per frame), so the LEDs visibly blink instead of finishing inside one
synchronous run.

## Framebuffer (display via the VideoCore)

The mailbox property channel also serves as the display: the `fb`
program sends `SET_PHYSICAL_W/H`, `SET_VIRTUAL_W/H`, `SET_DEPTH` and
`SET_PIXEL_ORDER` tags, then `ALLOCATE_BUFFER` — the host carves
`0x200000` out of guest RAM (160x120x32, RGB byte order) and answers
`GET_PITCH` with `640`. The guest then writes pixels straight into the
framebuffer (little-endian `R | G<<8 | B<<16`):

```
> fb
fb: 160x120 pitch 640 @ 0x200000
pattern drawn
```

After a deterministic test pattern (red screen, blue border, yellow
ball) the guest runs a bouncing-ball animation paced by the system
timer. `fbRun()` advances slices on `requestAnimationFrame` (~16 ms per
frame, the guest never parks) and blits the framebuffer to a
`<canvas>` (scaled 3x, `image-rendering: pixelated`) after every
batch — a live display until Reboot.

## Interrupt controller (0x3F00B200)

The `irq` program demos host-assisted IRQ delivery. The BCM2837 legacy
interrupt controller window (0x3F00B200, real register layout) is
mapped read/write: the host refreshes PENDING1 (0x204) from device
state each slice and latches ENABLE_IRQS1 (0x210) when the guest arms
interrupts. The guest installs its own VBAR (vector table at
0x100280) and the host watches it: on a new delivery the next slice
starts at the vector instead of the guest's PC.

The guest never sees a real exception — no `eret` in this unicorn
build, and host writes to ELR_EL1 don't stick — so a delivery is:

1. host saves the interrupted PC (`irqElr`) and runs the next slice
   from `VBAR+0x280`;
2. the guest stub (`bl irq_handler_rust; movz/movk; str w0,[x1]; b .`)
   finishes the handler and writes a magic 1 to IC_IRQ_RET
   (0x3F00B22C, a host-only extension register);
3. the host sees the write at the next slice boundary, clears it and
   resumes the guest at `irqElr`.

IRQ 29 (system timer C1 match at 0x3F003014) drives a 1 s heartbeat;
IRQ 31 (UART RX slot non-empty) fires when a key arrives — the guest
handler prints `[irq key: '...']` and consumes the slot. Only one IRQ
can be in flight (nested delivery is gated on the handler's return):
in the same slice a return and a fresh pending bit can both appear —
the new IRQ is not vectored until the guest actually resumes at
`irqElr`, so the vector is never dropped. In the browser the guest
spins with IRQs unmasked, so `irqRun()` advances slices on rAF; keys
write the RX slot and the IRQ is delivered at the next slice boundary.

```
> irq
irq: BCM2835 interrupt controller @ 0x3F00B200
irq: timer C1 + UART RX armed, IRQ enabled
[irq #1 t+1s]
[irq key: 'H']
[irq #2 t+1s]
```

## Programs

| Program | What it does |
|---------|--------------|
| `shell` | prompt `> `, case-insensitive commands: `hi` → `HELLO`, `rpi` → `Raspberry Pi 3`, `help`, `ver` → `pi3-emu v1.0`, `time` → live timer, `mbox` → VideoCore property query, unknown/empty → `?`/prompt |
| `sum`   | enter: prints `sum(1..10) = 55` (u64 division in guest) |
| `fib`   | enter: prints fibonacci `0..12` (13 terms) |
| `smp`   | auto-runs: 4 cores launch through the mailbox, sum quarters of 1..100, mailbox tally, joined counter (Reboot to re-run) |
| `clock` | auto-runs: real 1 s timer sleep, C1 compare + M1 match, W1C-style clear (Reboot to re-run) |
| `gpio`  | auto-runs: LED knight-rider chase on pins 21..28, then polls BTN 29 (LED panel + button in the page) |
| `fb`    | auto-runs: allocates a 160x120x32 framebuffer via mailbox, pattern + bouncing-ball animation (live canvas in the page) |
| `irq`   | auto-runs: arms the legacy interrupt controller, timer C1 heartbeat every 1 s + a UART RX IRQ per key (type to see `[irq key: '...']`) |

Backspace (`⌫`) sends 0x7F; the guest unwrites its line buffer and the
host trims the display. The terminal auto-focuses on boot; an on-screen
keyboard is available for mouse/touch; a live panel shows PC, SP, MIPS,
steps, instructions and timings.

## Run

```sh
npm install
npm run dev        # vite dev server -> http://localhost:5173
```

## Tests (no browser needed — same wasm driven from node)

```sh
npm run smoke                    # ELF program boot + all sessions, exact match
node test/stats-probe.mjs        # PC/SP/MIPS after one slice of shell.elf
node test/smp-probe.mjs          # 4-core SMP run: launch, mailbox, counter, park
node test/clock-probe.mjs        # system timer: 1 s sleep, compare/match, clear
node test/mbox-probe.mjs         # VideoCore mailbox: full property query response
node test/gpio-probe.mjs         # GPIO: LED chase toggles + button press edges
node test/fb-probe.mjs           # framebuffer: mailbox allocation, pattern pixels, ball moves
node test/irq-probe.mjs          # interrupts: timer C1 IRQ 29 + UART RX IRQ 31 delivered, both handlers run
node test/branch-probe.mjs       # condition/branch encoding probes (15)
node test/csel-probe.mjs         # csel probe (8)
```

## Build

```sh
npm run build    # build.sh (cargo board + guest programs) + vite build
```

## Layout

```
programs/             Rust workspace: runtime lib + shell/sum/fib/smp guests
  runtime/src/lib.rs  putc/puts/putu/getc + panic handler (UART I/O)
  linker.ld           entry at 0x100000, KEEP _start
  _start in each bin  naked asm: sets SP, then b rust_main (host reg
                      writes are no-ops in this unicorn build)
  smp/                4-core demo: per-core _start entries (distinct SPs),
                      mailbox protocol, spinlocks, park bits
src/elf.js            ELF64 loader (PT_LOAD + bss zeroing)
src/main.js           browser host loop (run-until-idle scheduler)
board/src/lib.rs      board model (UART console FIFO only)
test/smoke.mjs        guest-driven end-to-end test (node)
public/programs/*.elf built guest programs (committed)
public/pi_board.wasm  built board (committed)
public/unicorn.js     unicorn.js all-arch build (committed)
dist/                 production bundle
```

## History

- M1 — unicorn.js AArch64 core + Rust wasm board, ARM32-era spike
- M2 — interactive UART console, host-scheduled guest keyboard echo
- M3 — command shell, host-dispatched, guest-printed responses
- M4 — real guest kernel: RX poll loop, echo, line buffer, dispatch,
  responses and prompt all in AArch64 guest code
- M5 — ELF loader: guest programs are cross-compiled Rust ELFs (shell,
  sum, fib) loaded into guest RAM, host runs until idle, program
  selector in the UI
- M6 — SMP: four AArch64 cores (per-core unicorn instances, partitioned
  RAM), host-arbitrated mailbox MMIO window, primary launches secondaries,
  spinlocks + shared counter + per-core reports (smp program)
- M7 — system timer: real BCM2837 timer window, host-refreshed wall-clock
  counter, compare/match bits, real 1 s guest sleep (clock program),
  `time` command in the shell
- M8 — VideoCore mailbox: property-tags request/response (channel 8),
  host answers board identity/clock queries, multi-byte values serialized
  properly (byte truncation fixed), `.mbox` linker section moves the
  buffer out of the TCG env-hazard zone, `mbox` command + probe
- M9 — GPIO: real BCM2837 GPIO window, host-arbitrated GPSET/GPCLR/
  GPLEV (write-1 latches + refreshed input levels), pull-down button,
  LED knight-rider chase timed by the system timer, rAF-paced chase so
  the page's LED panel visibly blinks, `gpio` program + probe
- M10 — framebuffer: the mailbox now runs a display — SET physical/
  virtual W/H, depth, pixel order, ALLOCATE_BUFFER (host carves
  `0x200000` out of guest RAM) and GET_PITCH tags; the `fb` program
  draws a deterministic pattern then a bouncing-ball animation paced by
  the system timer; `fbRun()` advances slices on rAF and blits the
  framebuffer to a 3x pixelated canvas — a live display, `fb` program +
  probe
- M11 — interrupts: legacy BCM2837 interrupt controller window
  (0x3F00B200), host-refreshed PENDING1/latched ENABLE_IRQS1, guest
  VBAR + vector table; host-assisted delivery (slice starts at
  `VBAR+0x280`, stub signals IC_IRQ_RET, host resumes at the saved PC)
  since there is no `eret`; timer C1 heartbeat (IRQ 29) + UART RX key
  IRQ (IRQ 31), one-IRQ-in-flight gating that never drops a vector on
  the return+pend same-slice edge; `irq` program + probe