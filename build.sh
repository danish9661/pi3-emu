#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

cargo build --release --target wasm32-unknown-unknown -p pi-board

cp target/wasm32-unknown-unknown/release/pi_board.wasm public/pi_board.wasm
cp node_modules/@alexaltea/unicorn-js/dist/unicorn.js public/unicorn.js
rm -f public/unicorn.wasm

echo "built: public/pi_board.wasm, public/unicorn.js (all-arch, wasm embedded)"