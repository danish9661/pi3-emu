# sab-toggle

Reusable **SharedArrayBuffer on/off switch** for any web project. One file, zero
dependencies, no build step. Lets users pick multi-threaded vs single-threaded
execution without breaking on platforms that can't isolate (iOS Safari,
`file://`, VPNs stripping COOP/COEP).

```html
<script src="./sab-toggle.js"></script>
<select id="threads">
  <option value="auto" selected>threads: auto</option>
  <option value="on">threads: on</option>
  <option value="off">threads: off</option>
</select>
<script>
  SabToggle.bindSelect("#threads"); // init + persist + sync
  const mode = SabToggle.resolve();
  // mode.requested: 'auto'|'on'|'off'
  // mode.effective: 'multi'|'single'  <- boot this engine
  // mode.sab: true|false              <- shared memory usable?
  // mode.reason: human-readable why
  if (mode.effective === "multi") startPthreadEngine();
  else startSingleThreadEngine();
</script>
```

## Modes

| Pick | Meaning |
|------|---------|
| `auto` (default) | Multi-threaded engine when shared memory is usable, else the single-thread fallback. Safe for shared links — never a black screen. |
| `on` | Force multi-thread; fails loudly with the reason where isolation is missing. |
| `off` | Force single-thread; needs an ST build next to the MT one (see `pickVariant`), else you get a `{missing}` signal for your own fallback UI. |

Preference order: explicit argument › `?threads=` URL param › `#threads=` hash ›
`localStorage` (`sab.threads`) › `auto`.

## API

- `detect()` — never-throws capability probe (`{secure, isolated, hasSAB, sharedWasmOk, supported, reason}`).
- `getPreference(explicit?)` / `setPreference(v)` — read/persist the pick.
- `decide(requested)` — pure decision for hosts with their own sources (parent window, native shell).
- `resolve(prefer?)` — `decide(getPreference(prefer))`.
- `ensureIsolation(swUrl, {want, coi}?)` — register a COI service worker only when threads could be on (skips the reload loop otherwise). Returns `"already"|"registered"|"skipped"`.
- `probeFile(url)` — HEAD probe that rejects SPA-fallback/404 HTML pages.
- `pickVariant({mode, isSt, stEntry, stPage})` — resolves to `{url}` (navigate), `{missing}` (show build panel), or `{boot}` (boot current engine). Pass `stEntry` as a bootability sentinel your build creates only after a verified boot.
- `bindSelect(sel, onChange?)` — one-line dropdown wiring.
- `describe(mode, {isSt,…}?)` — badge strings (`{text, single}`).

Listens/emits `sab:mode` and `sab:preference` CustomEvents.

## Test

```sh
npm test   # node smoke test (no browser): API shape + decision matrix
```

## License

MIT — see the repository root LICENSE.
