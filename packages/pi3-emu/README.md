# pi3-emu

Headless **Raspberry Pi 3 (BCM2837) emulator** for node and browsers — the
rp2040js-style engine half of the pi3-emu project. An AArch64 CPU core
(unicorn.js, vendored — no install-time build) plus JavaScript device models
with **real BCM2837 register layouts**, driven in bounded slices with
host-arbitrated MMIO sync. No DOM required.

```js
import { Pi3Emulator, loadUnicorn } from 'pi3-emu';
import { readFileSync } from 'node:fs';

const ucMod = await loadUnicorn(); // browsers: await window.MUnicorn()
const emu = new Pi3Emulator(ucMod);
await emu.loadFirmware(readFileSync('./firmware/shell.elf'));
emu.runUntilIdle();                     // boot banner + prompt
emu.sendLine('hi'); emu.runUntilIdle(); // guest answers HELLO
console.log(emu.consoleText);
```

## What's modeled

System timer (`0x3F003000`), PL011 UART0 console (`0x3F201000`), GPIO with
LED outputs + button input + edge IRQs (`0x3F200000`), legacy interrupt
controller (`0x3F00B200`), local interrupt block (`0x40000000`), AUX mini
UART, BSC/I2C, SPI0, PWM, SDHCI, DMA, RNG, clock manager, I2S, USB, UART2–5 —
plus host-assisted MMU and a real-ELF loader. See `src/` (one module per
device, same files the interactive site runs).

## Wokwi-style parts

`attachI2c()` / `attachSpi()` accept an `onBridgeData` callback and return a
`bridgeRx()` injector: device traffic flows to your UI, responses flow back
into the guest. `examples/i2c-temp-sensor/` is a complete reference part
(DOM widget + virtual sensor). GPIO LEDs/buttons are first-class:
`emu.ledLevels()`, `emu.setButton(down)`.

```js
const i2c = emu.attachI2c(0x3f804000, (msg) => {
  if (msg.type === 'i2c-read') i2c.bridgeRx({ bytes: [26, 0] }); // 26 C
});
```

## Firmware

Like flashing, minus the flash chip: the Pi 3 has no onboard flash, so
"firmware" is just bytes loaded into RAM (`loadFirmware`) or an SD image.
Bring your own bare-metal AArch64 ELF (linked at `0x100000`, `_start` sets
SP), or start from `firmware/shell.elf` + `firmware/i2c.elf`.

## Scope notes (v0.1)

- Single core; SMP/Linux (qemu-wasm path) stay in the main pi3-emu app.
- No VideoCore mailbox model: the mailbox window is zero-mapped, so guests
  issuing mailbox tags spin (run loops stay bounded).
- Interrupts: host-assisted `IRQ_RET` delivery by default; `realIrq: true`
  for local-block guests.

## Test

```sh
npm test   # boots shell.elf (expects HELLO) + i2c sensor reads, headless
```

## License

MIT — see the repository root LICENSE.
