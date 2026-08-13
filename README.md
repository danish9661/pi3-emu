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

## Programs

| Program | What it does |
|---------|--------------|
| `shell` | prompt `> `, case-insensitive commands: `hi` → `HELLO`, `rpi` → `Raspberry Pi 3`, `help`, `ver` → `pi3-emu v1.0`, unknown/empty → `?`/prompt |
| `sum`   | enter: prints `sum(1..10) = 55` (u64 division in guest) |
| `fib`   | enter: prints fibonacci `0..12` (13 terms) |

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
node test/branch-probe.mjs       # condition/branch encoding probes (15)
node test/csel-probe.mjs         # csel probe (8)
```

## Build

```sh
npm run build    # build.sh (cargo board + guest programs) + vite build
```

## Layout

```
programs/             Rust workspace: runtime lib + shell/sum/fib guests
  runtime/src/lib.rs  putc/puts/putu/getc + panic handler (UART I/O)
  linker.ld           entry at 0x100000, KEEP _start
  _start in each bin  naked asm: sets SP, then b rust_main (host reg
                      writes are no-ops in this unicorn build)
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