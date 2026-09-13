#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

# pi-cpu (the only CPU core): Rust -> wasm browser build. public/pi_cpu is
# gitignored (wasm-pack writes its own .gitignore there); every build
# regenerates it, so a missing wasm-pack is a hard error, not a fallback.
wasm-pack build cpu --target web --out-dir ../public/pi_cpu
bash build-programs.sh
bash build-kernel.sh

echo "built: public/pi_cpu (pi-cpu wasm), public/programs/*.elf"
