#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/programs"

# Build each guest program as a bare-metal AArch64 ELF and copy to public/programs.
for p in shell sum fib; do
  cargo build --release -p "$p"
  cp "target/aarch64-unknown-none/release/$p" "../public/programs/$p.elf"
  echo "built: public/programs/$p.elf"
done
