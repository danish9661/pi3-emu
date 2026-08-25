#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

cargo build --release --target wasm32-unknown-unknown -p pi-board

cp target/wasm32-unknown-unknown/release/pi_board.wasm public/pi_board.wasm
# Rebuilt unicorn.js fork (patched: arm64_set_irq, timer_tick, debug exports)
# from /tmp/opencode/unicornjs-src via `python3 build.py --release`. On CI (and
# any machine without the fork source) that path is absent, so fall back to the
# committed public/unicorn.js rather than failing the whole build.
UNICORN_DIST="${UNICORN_DIST:-/tmp/opencode/unicornjs-src/dist}"
if [ -f "$UNICORN_DIST/unicorn.js" ]; then
  cp "$UNICORN_DIST/unicorn.js" public/unicorn.js
  rm -f public/unicorn.wasm
elif [ -f public/unicorn.js ]; then
  echo "[build] $UNICORN_DIST/unicorn.js not found; using committed public/unicorn.js"
else
  echo "[build] WARNING: unicorn.js unavailable; bare-metal guests will not work" >&2
fi
bash build-programs.sh

echo "built: public/pi_board.wasm, public/unicorn.js (patched fork, wasm embedded), public/programs/*.elf"