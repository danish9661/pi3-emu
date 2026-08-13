#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

cargo build --release --target wasm32-unknown-unknown -p pi-board

cp target/wasm32-unknown-unknown/release/pi_board.wasm public/pi_board.wasm
# Rebuilt unicorn.js fork (patched: arm64_set_irq, timer_tick, debug exports)
# from /tmp/opencode/unicornjs-src via `python3 build.py --release`.
UNICORN_DIST="${UNICORN_DIST:-/tmp/opencode/unicornjs-src/dist}"
cp "$UNICORN_DIST/unicorn.js" public/unicorn.js
rm -f public/unicorn.wasm
bash build-programs.sh

echo "built: public/pi_board.wasm, public/unicorn.js (patched fork, wasm embedded), public/programs/*.elf"