#!/usr/bin/env bash
# M52 kernel build: bare-metal aarch64 kernel at 0x80000 -> public/programs.
set -euo pipefail
cd "$(dirname "$0")/ports/rpi-kernel"

cargo build --release
cp "target/aarch64-unknown-none/release/rpi-kernel" "../../public/programs/rpi-kernel.elf"
echo "built: public/programs/rpi-kernel.elf"
