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

## MMU (0x3F00D000, host-assisted virtual memory)

This unicorn build cannot run a guest with the MMU on: writing
SCTLR_EL1 with bit 0 (M) set raises a CPU exception and MAIR_EL1
reads/writes are not implemented (both verified deterministically with
page-aligned Uint8Array buffers — plain JS arrays are silently
corrupted by `mem_write`). The `mmu` program demos host-assisted
translation instead: the guest still does the real work — it builds a
classic 4-level page table in RAM and enables the MMU by writing
`rootPa | 1` to the MMU_CTL window — and the host walks those tables
(src/mmu.js):

```
VA 0x00000000 - 0x3FFFFFFF  1G block  -> PA 0x00000000 (identity)
VA 0x80000000 - 0x801FFFFF  2M block  -> PA 0x00300000 (alias)
```

At enable, the host maps every non-identity block at its VA with a
shadow copy of the PA contents and installs a `HOOK_MEM_WRITE` mirror
that keeps the shadow and the PA coherent in both directions; identity
blocks need no shadow. Writes to unmapped VAs fault naturally
(`UC_ERR_READ_UNMAPPED`), so the host simply ignores the `emu_start`
error and resumes from the faulting PC — no retry logic needed.

The guest polls the reflected MMU_CTL status until the host has
enabled translation (an MMIO handshake like `getc`), then stores/
loads data through the alias, copies a 5-word function to PA 0x200200
and calls it at VA 0x80000200 (executing from the shadow copy), and
verifies every result. Like the clock, the mmu guest has silent phases
(building tables, waiting for enable) that would trip the run-until-
idle heuristic, so it finishes by writing a magic 1 to MMU_DONE
(0x3F00D004, host-only extension, same protocol as TMR_DONE) and
`runUntilMmuDone()` stops when it sees it.

```
> mmu
mmu: host-assisted MMU @ 0x3F00D000
mmu: tables at 0x280000, enabling...
mmu: enabled
mmu: alias write -> PA read OK
mmu: PA write -> alias read OK
mmu: shadow-code call OK
mmu: all checks passed
mmu: parked
```

## DMA (0x3F007000, host-arbitrated controller)

The `dma` program demos the BCM2835 DMA controller with the host
performing the transfers (the guest still does the real work: it writes
control blocks, programs the channel and arms the IRQ). Channel 0 has
the real register layout — CS at +0x00 with ACTIVE/END/INT bits,
CONBLK_AD at +0x04 — plus the real DMA_ENABLE register at 0x3F00E050.
The guest builds a 3-CB chain in RAM and starts it by writing
CS.ACTIVE; the host walks the chain between slices (src/dma.js), copies
or fills the buffers, latches CS.END and raises CS.INT — the INTEN bit
(31, a documented host extension) on the final CB drives the IC's DMA0
line (bit 16), and the M11 delivery path vectors the guest:

```
CB0 0x284000: copy 64 bytes  0x285000 -> 0x286000 (pattern 0x5a+i)
CB1 0x284020: copy 32 bytes  0x286000 -> 0x287000 (relay)
CB2 0x284040: fill  16 bytes 0x288000 with the byte at 0x284080
             (SRC_IGNORE, final CB sets INTEN -> completion IRQ)
```

Because the host re-asserts the DMA0 line until the guest clears CS.INT,
the guest's own code must stay interrupt-safe: `poll_end` and a small
`delay_spin` keep their state in callee-saved registers (the delivery
clobbers x0-x18), the vector glue preserves x30 (the `bl` to the Rust
handler would otherwise make the guest's next `ret` jump into the glue),
and the host masks the DMA0 pending bit once INT is cleared so a stale
window can't re-deliver after the guest has acknowledged. The guest
then verifies all three destinations and parks via the DMA_DONE
protocol (0x3F00E054, like TMR_DONE/MMU_DONE).

```
> dma
dma: BCM2835 DMA controller @ 0x3F007000
dma: channel 0 enabled, IRQ 16 armed, chain at 0x284000
[dma irq] completed
dma: chain done (END set)
dma: full copy OK (64 bytes)
dma: relay copy OK (32 bytes)
dma: fill OK (SRC_IGNORE, 16 bytes)
dma: all checks passed
dma: parked
```

## PWM audio (0x3F20C000, host-arbitrated controller)

The `pwm` program makes the emulator audible: the guest drives the
BCM2835 PWM controller in FIFO mode and the browser plays the samples
through WebAudio. The guest enables channel 1 (PWEN1) with USEF1 (FIFO
mode) + MSEN1 (M/S enabled), clears the FIFO on the CLRF1 edge, then
pushes square-wave samples for "Twinkle Twinkle Little Star" — one
fixed-point phase accumulator per note, all integer math — paced by the
FULL1/EMPT1 handshake the host refreshes in STA (the FIFO is a real
device: it fills, the guest waits, the host drains it between slices).

The controller's write-mask model (src/pwm.js): CTL level bits are
latched from guest writes and reflected back (write-mask semantics,
like the other windows), STA.FULL1/EMPT1 are derived from the FIFO
depth, and FIFO/DAT1 writes are captured with a range-limited
`HOOK_MEM_WRITE` — unicorn callbacks carry the written value, so two
identical samples in a row can't be missed the way a window diff would.
Each word's low 16 bits are a signed sample (the documented convention
for this model); the host drains 64 samples per slice into a ring (the
depth 256 absorbs one slice's burst, since the guest can only observe
FULL1 at slice boundaries) and the browser's ScriptProcessor pulls the
ring to the speakers at 44.1 kHz. The guest finishes with the usual
explicit-done protocol (PWM_DONE at +0x54, like TMR_DONE/MMU_DONE).

```
> pwm
pwm: BCM2835 PWM controller @ 0x3F20C000
pwm: FIFO mode enabled (USEF1|MSEN1), FIFO cleared
pwm: note 262 Hz
pwm: note 262 Hz
pwm: note 392 Hz
pwm: note 392 Hz
pwm: note 440 Hz
pwm: note 440 Hz
pwm: note 392 Hz
pwm: note 349 Hz
pwm: note 349 Hz
pwm: note 330 Hz
pwm: note 330 Hz
pwm: note 294 Hz
pwm: note 294 Hz
pwm: note 262 Hz
pwm: all notes played (84672 samples)
pwm: parked
```

## I2C (0x3F804000, host-arbitrated sensor)

The `i2c` program reads a host-played sensor: the BCM2835 BSC master at
0x3F804000, pure window model like the DMA/IC (src/i2c.js) — when the
guest raises C.ST the host snapshots the FIFO window (DLEN bytes), runs
the slave, and for reads loads the response back into the FIFO window.
The slave is a classic sensor sequence: write the register address, read
the data. Registers: 0x00 WHO_AM_I = 0x68, 0x10 TEMP = 26 C (2 bytes),
0x20 COUNTER increments per read (1, 2, 3). The guest's three read
pairs verify all three; the stale-window race (a poll started in the
same slice as its own write can read the previous transfer's S.DONE)
is handled by a pre-wait loop like the SPI/SD guests. I2C_DONE at
+0x54 (explicit-done protocol).

```
> i2c
i2c: BCM2835 BSC master @ 0x3F804000
i2c: WHO_AM_I = 0x68
i2c: temp = 26 C
i2c: counter = 3 (reads 1, 2, 3)
i2c: all checks passed
i2c: parked
```

## SPI (0x3F204000, host-arbitrated flash slave)

The `spi` program drives the BCM2835 SPI0 master (src/spi.js) with the
host playing an SPI slave: a flash chip answering the JEDEC ID command
(0x9F) with 0xEF 0x40 0x18. The guest runs a real transaction: select
CS0, push the command bytes into the FIFO, raise TA, poll CS.DONE, read
the response, CLEAR the FIFOs — twice, and requires the two responses
to be identical (proves the CLEAR resets the session). The FIFO and CS
registers are write-hooked: a window diff only sees the last write of a
slice, so a CLEAR followed by TA in one slice would be invisible, and
identical FIFO pushes can't be told apart. (Also found: this unicorn
build's hook range end is inclusive, so the CS hook would fire for the
adjacent FIFO — the hooks guard by address.) SPI_DONE at +0x54.

```
> spi
spi: SPI0 master @ 0x3F204000
spi: JEDEC ID = 0xEF 0x40 0x18 (Winbond W25Q128)
spi: both transactions identical (CLEAR resets OK)
spi: all checks passed
spi: parked
```

## Mini UART (0x3F215000, second console)

The `uart1` program adds a second console: the BCM2835 AUX mini UART at
0x3F215000, configured like a real driver (AUX_ENABLES, 8N1 LCR, TX
enable, baud) and written through MU_IO with LSR bit 5 TX-empty pacing.
Its chars reach the same terminal tagged `[u1] ` once per line. MU_IO is
write-hooked so UART1 chars interleave with the primary UART's chars in
the exact order the guest wrote them (a slice diff would reorder chars
written in the same slice — the first symptom was a UART0 `\n` landing
after the first UART1 `u`); the primary UART got the same hook, and the
diff pump now only clears the slots (the hookless SMP cores still get
pushed by the pump). Output-only: input stays on UART0. The guest parks
on getc, so it runs under the run-until-idle heuristic.

```
> uart1
uart1: mini UART (AUX) @ 0x3F215000
uart1: UART0 console is active
uart1: starting UART1 diagnostics
[u1] uart1: hello from the mini UART
[u1] uart1: LSR TX-empty pacing works
[u1] uart1: diag line 3/3
uart1: UART1 diagnostics complete
uart1: parked
```

## SD card (0x3F300000, host-arbitrated SDHCI + FAT12)

The `sd` program boots from a microSD card: the BCM2835 SDHCI (EMMC)
controller at 0x3F300000, backed by a host-played 5-sector FAT12 disk
image holding one file, HELLO.TXT ("hello from the SD card"). The guest
runs the real init sequence (CMD0/CMD8/ACMD41/CMD2/CMD3/CMD7) with
write-1-to-clear CMD_COMPLETE polling, then CMD17 single-block reads,
parses the boot sector and root directory, finds HELLO.TXT and prints
it. CMD is write-hooked (the same CMD value recurs, so a window diff
could not detect repeats); the block is exposed at +0x100 (the real
controller pops the data FIFO at +0x20 on every read — a plain window
can't pop, so the guest walks the buffer addresses; the naive first
version exposed the block at +0x20 and collided with INTERRUPT at
+0x30). SD_DONE at +0x54.

```
> sd
sd: SDHCI (EMMC) @ 0x3F300000, FAT12 card
sd: CMD8 echo OK (v2, 3.3V)
sd: ACMD41 ready (high capacity)
sd: RCA = 0x1234
sd: card selected (R1 ready)
sd: FAT12 boot sector OK (512B, 2 FATs)
sd: HELLO.TXT found in root directory
sd: file HELLO.TXT: sd: "hello from the SD card"
sd: payload matches
sd: all checks passed
sd: parked
```

## UART0 — the PL011 (0x3F201000, real BCM2837 layout)

The main console is now the real PL011: the guest configures IBRD/FBRD/
LCRH/CR like a real driver, polls FR for TX/RX flow control, reads and
writes DR, and RXINTR (IMSC bit 4) drives the interrupt controller's
IRQ 57 line (bank 1, bit 25). This replaces the original console "slots"
window that the runtime used to know about. TX: a DR write is captured
by a HOOK_MEM_WRITE and emitted at write time (gated on CR.UARTEN read
live from the window), so console chars keep the guest's exact write
order. RX: the host pushes key bytes into a small FIFO and pre-loads
the head byte into the DR cell before every slice; delivery is split
across the FR and DR read hooks — this unicorn build runs a read hook
*before* the CPU latches the read, so a hook may never rewrite the
register being read (the FR hook only re-mirrors the DR cell with the
current FIFO head; the DR hook pops the FIFO but leaves the cell alone).
The `uart0` program verifies the whole path: config latched, FR TX-ready,
RXINTR armed, a typed key fires `RXINTR` (MIS 0x10) through the IC and
is echoed via a DR read.

```
> uart0
uart0: BCM2837 PL011 @ 0x3F201000
uart0: IBRD 1 FBRD 28 (115200 @ 3 MHz), LCRH 0x70, CR 0x301
uart0: FR shows TXFE set, TXFF clear — TX ready
uart0: RXINTR armed (IMSC bit 4) -> IRQ 57 — type a key
uart0: RXINTR fired (MIS 0x10)
uart0: [rx 'x']
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
| `mmu`   | auto-runs: builds 4-level page tables in RAM, host walks them on MMU_CTL (0x3F00D000), alias VA 0x80000000→PA 0x200000, shadow-code call (Reboot to re-run) |
| `dma`   | auto-runs: 3-CB DMA chain (copy/relay/fill) performed by the host between slices, CS.END + CS.INT latched, completion IRQ 16 handled, destinations verified (Reboot to re-run) |
| `pwm`   | auto-runs: FIFO-mode PWM — the guest generates a square-wave "Twinkle" melody paced by the FULL1/EMPT1 handshake; the host drains the FIFO and plays it through WebAudio (Reboot to re-run) |
| `i2c`   | auto-runs: reads a host-played sensor over BSC: WHO_AM_I, TEMP, and a COUNTER that increments per read (Reboot to re-run) |
| `spi`   | auto-runs: two identical SPI0 transactions against a flash slave; JEDEC ID 0xEF 0x40 0x18, CLEAR-reset proven by r1 == r2 (Reboot to re-run) |
| `uart1` | auto-runs: second console on the AUX mini UART, output tagged `[u1] ` (Reboot to re-run) |
| `sd`    | auto-runs: SDHCI init sequence + FAT12 card read; parses boot sector/root dir, prints HELLO.TXT (Reboot to re-run) |
| `uart0` | auto-runs: real PL011 console — verifies the baud-rate config, FR flow control, RXINTR → IRQ 57 and RX echo (type a key to see `[rx 'x']`, Reboot to re-run) |

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
node test/mmu-probe.mjs          # MMU: host-assisted translation, alias + shadow-code checks
node test/dma-probe.mjs          # DMA: host-performed 3-CB chain, END/INT latch, IRQ 16 delivered, dst verified
node test/pwm-probe.mjs          # PWM: FIFO-mode config, FULL1/EMPT1 handshake, exact 84672-sample stream
node test/i2c-probe.mjs          # I2C: sensor WHO_AM_I/TEMP/COUNTER reads, slave register select
node test/spi-probe.mjs          # SPI: JEDEC ID transaction, CLEAR resets, r1 == r2
node test/uart1-probe.mjs        # UART1: mini UART config, [u1]-tagged stream, TX-empty pacing
node test/sd-probe.mjs           # SD: init sequence, FAT12 boot/root parse, HELLO.TXT payload
node test/uart0-probe.mjs        # UART0: PL011 config latched, FR TX-ready, RXINTR via IC, RX echo
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
  mmu/                virtual memory demo: builds 4-level page tables,
                      enables via MMU_CTL, tests the alias + shadow code
  dma/                DMA demo: 3-CB chain, ACTIVE start, IRQ 16 handler
  pwm/                PWM audio demo: FIFO-mode square-wave melody
  i2c/                I2C demo: BSC master, host-played sensor reads
  spi/                SPI demo: SPI0 master, flash slave JEDEC ID
  uart1/              mini UART demo: second console tagged [u1]
  sd/                 SD demo: SDHCI init + FAT12 card, prints HELLO.TXT
  uart0/              PL011 demo: baud config, FR flow control, RXINTR -> IRQ 57, RX echo
src/elf.js            ELF64 loader (PT_LOAD + bss zeroing)
src/mmu.js            host-assisted MMU: table walk, shadow mapping, mirror
src/dma.js            host-arbitrated DMA: CB chain walk + transfer engine
src/pwm.js            host-arbitrated PWM: FIFO model, drain ring, write hook
src/i2c.js            host-arbitrated I2C: BSC window, sensor slave
src/spi.js            host-arbitrated SPI: SPI0 window, flash slave
src/uart1.js          mini UART model: write hook emits chars at write time
src/uart0.js          PL011 model: DR/FR/RIS/MIS windows, RX FIFO, RXINTR -> IRQ 57
src/sdhci.js          host-arbitrated SDHCI: block buffer, FAT12 image
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
- M12 — MMU: unicorn cannot run a guest with SCTLR.M set (verified:
  the sysreg write raises an exception and MAIR_EL1 is unimplemented),
  so translation is host-assisted; the guest builds real 4-level page
  tables and enables via MMU_CTL (0x3F00D000); the host walks the
  tables, maps non-identity blocks with shadow copies + a two-way
  write mirror (HOOK_MEM_WRITE), unmapped VAs fault naturally; the
  guest handshakes on the reflected status and finishes with an
  MMU_DONE write (0x3F00D004, like TMR_DONE); alias VA 0x80000000→
  PA 0x200000 + shadow-code call; `mmu` program + probe
- M13 — DMA: BCM2835 DMA channel 0 window (0x3F007000, real CS/
  CONBLK_AD layout + the real DMA_ENABLE at 0x3F00E050); the host
  performs the chain between slices; the guest builds a 3-CB chain
  (copy/relay/fill), INTEN (host extension, bit 31) on the final CB
  drives the IC's DMA0 line, END/INT latched in CS and INT cleared by
  write-mask; interrupt-safety fixes: the vector glue preserves x30
  (the `bl` to the Rust handler must not redirect the guest's next
  `ret`), poll/delay loops keep their state in callee-saved registers,
  and the host masks the DMA0 pending bit once INT is cleared (no
  stale-window re-delivery); DMA_DONE (0x3F00E054) protocol; `dma`
  program + probe
- M14 — PWM audio: BCM2835 PWM window (0x3F20C000), FIFO-mode model —
  CTL level bits latched with write-mask semantics, STA.FULL1/EMPT1
  derived from the FIFO depth, CLRF1 edge clears, FIFO/DAT1 pushes
  captured by a range-limited HOOK_MEM_WRITE (a window diff could not
  tell two identical samples apart); the pwm guest generates a
  square-wave "Twinkle Twinkle Little Star" with fixed-point phase
  accumulators (integer-only), paced by the FULL1/EMPT1 handshake (the
  FIFO really fills and the guest waits — depth 256 absorbs one slice's
  burst because FULL1 is only observable at slice boundaries); the host
  drains 64 samples/slice into a ring (low 16 bits = signed sample) and
  the browser plays it through WebAudio (ScriptProcessor pull at
  44.1 kHz, created on the Run click gesture); PWM_DONE (0x3F20C054)
  protocol; `pwm` program + probe + E2E
- M15 — I2C: BCM2835 BSC master window (0x3F804000) with a host-played
  sensor slave — when the guest raises C.ST the host snapshots the FIFO
  window, runs the slave (write the register address, read the data),
  and loads the response back; WHO_AM_I = 0x68, TEMP = 26 C, COUNTER
  increments per read (1, 2, 3); the guest pre-waits before each read
  because a poll started in the same slice as its own write can see the
  previous transfer's S.DONE (stale-window race); I2C_DONE (0x3F804054)
  protocol; `i2c` program + probe + browser E2E
- M16 — SPI: BCM2835 SPI0 master window (0x3F204000) with the host
  playing a flash slave answering the JEDEC ID command (0x9F → 0xEF
  0x40 0x18); the guest runs a real transaction — select CS0, push the
  command into the FIFO, raise TA, poll CS.DONE, read, CLEAR — twice,
  and requires identical responses (proves CLEAR resets the session);
  FIFO/CS are write-hooked because a window diff only sees the last
  write of a slice and identical FIFO pushes can't be told apart;
  discovered the hook range end is inclusive (the CS hook fired for the
  adjacent FIFO — guarded by address); SPI_DONE (0x3F204054); `spi`
  program + probe + browser E2E
- M17 — Mini UART: second console on the BCM2835 AUX mini UART
  (0x3F215000, real AUX_ENABLES/LCR/baud config, LSR TX-empty pacing),
  tagged `[u1] ` once per line; MU_IO is write-hooked so UART1 chars
  interleave with UART0 chars in the guest's exact write order (a slice
  diff reorders chars written in the same slice — first symptom: a
  UART0 `\n` landing after the first UART1 `u`); the primary UART got
  the same write hook and the diff pump now only clears the slots (the
  hookless SMP cores still get pushed by the pump); `uart1` program +
  probe + browser E2E
- M18 — SD card: BCM2835 SDHCI (EMMC) window (0x3F300000) backed by a
  host-played 5-sector FAT12 disk image with HELLO.TXT; the guest runs
  the real init sequence (CMD0/CMD8/ACMD41/CMD2/CMD3/CMD7, write-1
  CMD_COMPLETE clears) then CMD17 single-block reads and parses the
  boot sector + root directory; CMD is write-hooked (the same CMD value
  recurs — a window diff could not detect repeats); the 512-byte block
  buffer sits at +0x100 because the real controller pops the data FIFO
  at +0x20 on every read — a plain window can't pop, and the first
  attempt at +0x20 collided with INTERRUPT at +0x30; FAT12 cluster low
  word read at dir entry +20; SD_DONE (0x3F300054); `sd` program +
  probe + browser E2E
- M19 — PL011 UART0: the main console is the real BCM2837 PL011
  (0x3F201000), replacing the console "slots" window — the guest
  configures IBRD/FBRD/LCRH/CR like a real driver, polls FR for TX/RX
  flow control, and RXINTR (IMSC bit 4) drives the interrupt
  controller's IRQ 57 line (bank 1, bit 25); TX is a DR write hook
  gated on CR.UARTEN, RX is a small FIFO whose head byte is pre-loaded
  into the DR cell each slice, delivered across the FR and DR read
  hooks — this unicorn build runs a read hook *before* the CPU latches
  the read, so a hook that rewrites the register being read hands the
  guest the post-hook value (first symptom: every key came back as
  0x00); the runtime lib and all programs moved to PL011 putc/getc;
  the irq guest arms IMSC and its vector glue now preserves the full
  register file (an IRQ can land mid-puts; the old glue saved only
  x29/x30); the probes resumed at a `lastPc` trace instead of the PC
  register — a boundary between `ldrb w15,[x0],#1` and its `str`
  re-executed the post-indexed load and skipped one byte per puts
  (first symptom: the irq banner lost its `:`); `uart0` program +
  probe + browser E2E
- M20 — the Linux boot project begins. Phase 1 rebuilt the unicorn.js
  fork (github.com/AlexAltea/unicorn.js @ 8028ec43, `python3 build.py
  --release`) with real exception injection: `uc_arm64_set_irq` asserts/
  de-asserts CPU_INTERRUPT_HARD, `uc_arm64_timer_tick` advances the ARM
  generic timer (guest TVAL → CVAL, ISTATUS + IRQ at CNTFRQ 19.2 MHz),
  `uc_arm64_debug` probes the IRQ lines/DAIF/PC; the delivery bug was
  `arm_cpu_do_interrupt` taking the AArch32 entry path (SCR_EL3.RW
  defaults 0) so PC never reached the vector — the A64 path is forced
  under TARGET_AARCH64; the `irqcore` guest verifies 13/13 (vector
  entry at VBAR+0x280, ELR_EL1/SPSR_EL1 saved, DAIF.I set, eret round
  trip, level semantics: a still-high line re-triggers after eret).
  Phase 2a: the BCM2836 local interrupt block (0x40000000) — CONTROL/
  PRESCALER/GPU+FIQ routing/per-core CORE_* registers with the real
  source layout (0 CNTPS, 1 CNTPNS, 2 CNTHP, 3 CNTV, 4-7 mailbox, 8 GPU,
  9 PMU, 10 AXI, 11 local timer); arch-timer bits are *reported* in the
  source register but never drive the host line (the gt path asserts/
  de-asserts CPU_INTERRUPT_HARD internally in real time — a host-side
  slice-boundary line would re-trigger after eret); the host line
  tracks GPU/PMU/AXI/local-timer/mailbox, delivered through
  `uc_arm64_set_irq` to a real vector; the `lirq` guest delivers
  CNTPNS (gt path) and then a system-timer compare (IRQ 29 → legacy IC
  → GPU line → local block bit 8 → real vector) whose `str wzr` TMR_CS
  write is a real-time hook that de-asserts the line, so no re-entry;
  the browser ticks the arch timer at the real 19.2 MHz rate during
   lirq runs; `lirq` program + probe (14/14) + browser E2E, full
   19-probe regression green
- M21 — the REAL MMU runs in the rebuilt core. The `mva` guest enables
  SCTLR_EL1.M/C/I with genuine 4K-granule LPAE stage-1 tables (T0SZ=25,
  TTBR0_EL1 at 0x280000, MAIR attr0=0xFF), keeps executing through the
  identity-mapped code path, stores via a 2M-block alias
  (L1B[0]=0x200401, VA 0x80000000 → PA 0x200000) and verifies both PAs
  agree (PASS, probe 7/7). The hunt was a walk that faulted on a valid
  chain: the alias went one level too deep — L0[2]→L1B[0]→L2[0] placed
  a 2M block at level 3, where bit1=0 is the reserved level-3 encoding
  → Translation fault. Fixing the guest tables dropped the L2
  indirection. Host side: `uc_arm64_debug` grew walk/fill/lpae
  diagnostics (sels 14-111: walk results, per-ptw-read addr/desc,
  per-fill va/access/mmu_idx/ret/fi.type, and an lpae exit ring with
  fault type, fault source (1 top-bits, 2 epd, 3 s2 startlevel, 4
  invalid desc, 5 AF, 6 permission), descriptor at fault, and exit
  level); the fork is QEMU ~4.2-era where `arm_el_is_aa64(env,1)` is
  false on bare-metal reset — the EL1 regime was being walked as
  AArch32 until cpu.h forced aa64 for el≤2; wrapper fix: the
  `uc_arm64_timer_tick` i64 param needs `BigInt(cntpct)` (ccall
  'number' throws), restoring `lirq` 14/14; full 19-probe regression
  green
- M22 — real legacy-IC IRQ semantics for the Linux-boot device set. A new
  `src/ic.js` models the BCM2835 interrupt controller (0x3F00B200) as a
  real 3-bank register file — IC_BASIC/IRQ1/IRQ2 pending, ENABLE_IRQS1/2
  + DISABLE_IRQS1/2, per-bank lines derived *fresh* from the device
  lines on every read (no stale windows) — with the real bank map: IRQ 1
  = timer bit 0, IRQ 7 = DMA0 bit 6 (kept at 1<<16 → bank-1 bit 16,
  host convention for our own DTB), IRQ 29 = system timer (bank-1 bit
  28 = basic bit 29), IRQ 57 = PL011 (bank-2 bit 25), IRQ 62 = SDHCI
  (bank-2 bit 30), IRQ 81/82 = GPIO banks 0/1 (bank-2 bits 17/18). The
  system-timer convention is fixed to real hardware: C1@0x10 → CS bit 1
  (the irq guest's timer line) and C3@0x18 → CS bit 3 (the lirq guest's
  Phase B — Linux's bcm2835_timer uses C3/IRQ 29). `src/gpio.js` is a
  full GPIO model — GPFSEL/GPSET/GPCLR W1S/W1C, host-driven GPLEV,
  GPEDS W1C, GPREN/GPFEN/GPHEN/GPLEN/GPAREN/GPAFEN, GPPUD — with
  host-side edge detection at slice boundaries and a bank IRQ line for
  any event bit covered by an enable; the guest's W1C goes through a
  write hook (a window pull would self-clear the host's own mirror —
  the probe's ev0 came back 0x0 with 0 deliveries until the hook
  handled it). PL011 gains TXIM (IRQ 57: TXINTR = TXFE&&TXIM, RXINTR =
  RXNE&&RXIM, de-armed in the handler so no post-eret storm) and real-
  time RX de-assert when the FIFO drains; SDHCI gains IRPT_EN/IRPT_MASK
  with the real line = (raw & IRPT_EN & IRPT_MASK). Guests move to the
  real offsets: irq = timer C1 (IRQ 1) + UART RX (IRQ 57), uart0 = RX
  then TXIM phases, gpio = GPREN on BTN 29 → IRQ 81 with a full vector
  + glue, lirq Phase B = C3@0x18 → IRQ 29. Delivery stays host-assisted
  for the legacy-IC guests (irqDeliver gated on DAIF.I clear, irqElr
  recorded at slice end, VBAR+0x280 vector, IRQ_RET magic at
  0x3F00B22C) and real for lirq — a discovered regression: the local
  block's `arm64_set_irq` line must fire only in LIRQ_MODE, because a
  real mid-slice entry into a legacy-IC guest left the host machinery
  without a resume point (`syncIrqRet resume 0`, the uart0 TX phase
  died). GPIO button fix: `getBtn` must return the pin bitmask
  (gpioBtn << 29). Verified: 20/20 probes + 9/9 browser E2E checks
  (phase2b-e2e.mjs covers irq/uart0/lirq/gpio)
- M23 — the Linux boot lands in the emulator itself (WIP: boot crashes
  inside the rebuilt core, fully instrumented). A new `linux` mode
  (`src/main.js`) loads a real arm64 kernel Image + stock
  bcm2837-rpi-3-b.dtb: RAM grows to 128 MB, the kernel boots per the
  arm64 protocol (x0 = DTB, x1..x3 = 0, MMU off, entry = base +
  header `text_offset`), every unmodeled peripheral window becomes a
  zero-return "black hole" map so stray driver probes fail gracefully
  instead of data-aboring, and the arch timer is ticked at the real
  19.2 MHz rate every slice with native IRQ delivery
  (CPU_INTERRUPT_HARD via the local block, like LIRQ_MODE) — the
  kernel never idles, so a fixed-budget rAF loop (`linuxRun`) replaces
  run-until-idle. PL011 gains a real earlycon fix: `DR` writes emit
  unconditionally (Linux's earlycon=pl011 writes the char without ever
  configuring CR.UARTEN), and the local block splits FIQ_ROUTING into
  the real PM_ROUTING_SET/PM_ROUTING_CLR pair. `test/linux-probe.mjs`
  boots the Image at the 2M-aligned 0x200000 (the old 0x80000 load
  made the kernel's mapping run 0x80000 off — everything faulted at
  slice 4243) with pre-seeded idmap aliases for the fork's 32-bit-
  truncated fetches. Status: the boot now reaches `early_security_init`
  (~slice 6484) where the rebuilt fork core aborts on an SVE/vector
  assert — the whole abort chain (call_indirect helper $1745 → assert
  wrapper $202, marker-instrumented across ~18 runs) is traced in
  AGENTS.md. Parallel proof-of-feasibility: a separate QEMU-wasm build
  (raspi3ap, same kernel 6.1.21-v8 + DTB + busybox initramfs) boots to
  a working busybox `~ #` shell under TCG — the root cause there was a
  dangling `/init -> busybox` symlink (busybox lives at bin/busybox)
  that tripped the rpi kernel's `init_eaccess` check and panicked in
  `prepare_namespace`; fixing `/init -> bin/busybox` boots to `~ #`. The
  unicorn-fork Linux path is abandoned (its TCI interpreter cannot
  translate NEON/SIMD — TCI has zero vector-op handlers), and this
  QEMU-wasm engine becomes the Linux boot path.
- M24 — QEMU-wasm becomes the live Linux engine. The `linux` program
  option now boots a real arm64 Linux (raspi3ap: BCM2837 / Pi 3 B+,
  4× Cortex-A53, 512 MB) via `ktock/qemu-wasm` inside an iframe
  (`runLinux()` in `src/main.js`) instead of the dead unicorn
  `linuxRun()` path. Harness in `public/linux/` (index.html, module.js,
  vendor/xterm.css). xterm + xterm-pty are vendored locally (xterm UMD →
  global `Terminal`, xterm-pty UMD → global `openpty`) so the console
  works offline; the console auto-activates by watching `.xterm-rows`
  for "Please press Enter to activate this console." and calling
  `xterm.focus()` then `xterm.paste("\r")`. Acceleration is MTTCG
  (`-accel tcg,tb-size=500,thread=multi -smp 4,sockets=4`) — single-
  thread triggers a `start is not a function` pthread-worker race. The
  whole app is cross-origin isolated (vite.config.js sets
  Cross-Origin-Opener-Policy: same-origin + Cross-Origin-Embedder-
  Policy: require-corp) so the same-origin iframe can use
  SharedArrayBuffer/pthreads; `public/linux/coi-serviceworker.js` is a
  no-op fallback for static hosts that don't send these headers.
  Verified headless: kernel boots to a busybox `~ #` on the serial
  console; typing round-trips. Tests: `test/linux-boot-vendored.mjs`,
  `test/linux-shell-interactive.mjs`, `test/linux-ui-integration.mjs`.
- M25 — From-source build + console polish. `scripts/build-linux.sh`
  rebuilds qemu-wasm + the raspi3ap kernel/rootfs from `ktock/qemu-wasm`
  via rootless podman, working end-to-end: emscripten CFLAGS/LDFLAGS are
  split (linker settings `-sWASM_BIGINT`/`-sMALLOC`/`-sASYNCIFY` belong
  in LDFLAGS), `--disable-werror` plus a two-pass configure that patches
  the `dtc` meson wrap's `werror=true` (else `-no-pie` is a hard error
  under emscripten), and a fakeroot-wrapped `mknod`/`mke2fs` so the
  rootfs image builds without `CAP_MKNOD`. The build is resumable
  (the build container and its `/build` object tree persist across
  invocations). NOTE: this ktock checkout lacks `PROXY_TO_PTHREAD`, so
  the from-source qemu is single-thread (~22 MB wasm vs the prebuilt's
  ~57 MB pthread/MTTCG binary) — the script copies its output into
  `public/linux-fromsrc/` and deliberately does NOT overwrite the live
  `public/linux/`; the prebuilt demo binary remains the deployed engine.
  `prepare_namespace`; fixed initramfs boots to the shell.