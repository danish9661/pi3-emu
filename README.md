# RPi 3 (BCM2837) emulator spike

A Raspberry Pi 3 emulator that runs entirely in the browser — an AArch64
CPU core (unicorn.js), a minimal board model, and a **real AArch64 guest
kernel** that boots, polls the UART, echoes keys, and answers commands.

```
+--------------------------------------------------------------+
| Browser (page)                                               |
|   term / status   <->   host loop (src/main.js)              |
|     delivers keystrokes, drains TX slots, draws console      |
+---------------------------+----------------------------------+
                            | WebAssembly
+---------------------------+----------------------------------+
| Board (Rust -> wasm)      |  board/src/lib.rs                |
|   UART console FIFO       |  runtime AArch64 assembler       |
|   guest kernel bytes      |  (build_kernel: 150 words @ 0x80000) |
+---------------------------+----------------------------------+
                            | Unicorn API
+---------------------------+----------------------------------+
| CPU: unicorn.js (QEMU TCG AArch64 core, wasm)                |
+--------------------------------------------------------------+
```

The guest kernel is a live process: after boot it parks in its RX poll
loop. On every keystroke the host resumes it from its current PC, runs a
bounded slice, and drains whatever the kernel wrote to the UART TX slots.
Echo, line buffering, command dispatch, responses and the prompt all run
inside the guest — the host only moves bytes.

## Device window (0x3F201000, 4 KiB)

- `+0x00` — 32 TX slots, one char per word (guest writes, host drains)
- `+0x80` — RX slot (host writes a byte, guest consumes)

## Commands

| Input   | Response         |
|---------|------------------|
| `HI`    | `HELLO`          |
| `RPI`   | `Raspberry Pi 3` |
| `HELP`  | `hi, rpi, help, ver` |
| `VER`   | `pi3-emu v1.0`   |
| other   | `?`              |

Commands are case-insensitive. Backspace (`⌫`) sends 0x7F; the guest
unwrites its line buffer and the host trims the display. The terminal
auto-focuses on boot; an on-screen keyboard is available for mouse/touch.

## Run

```sh
npm install
npm run dev        # vite dev server -> http://localhost:5173
```

## Tests (no browser needed — same wasm driven from node)

```sh
npm run smoke                    # full boot + all sessions, exact match
node test/branch-probe.mjs       # condition/branch encoding probes (15)
node test/csel-probe.mjs         # csel probe (8)
```

## Build

```sh
npm run build    # build.sh (cargo -> pi_board.wasm) + vite build
```

## Layout

```
board/src/lib.rs      board model + runtime AArch64 assembler + guest kernel
src/main.js           browser host loop
src/styles.css        console styling
test/smoke.mjs        guest-driven end-to-end test (node)
test/branch-probe.mjs test/csel-probe.mjs   isolated instruction probes
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
