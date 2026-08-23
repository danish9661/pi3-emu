#!/bin/sh
# Fetch the prebuilt qemu-wasm raspi3ap artifacts used by the "linux" boot
# mode in pi3-emu. These are large binaries (~84 MB) and are gitignored;
# this script restores them after a fresh clone.
#
# Source: https://github.com/ktock/qemu-wasm-demo-images  (raspi3ap/)
# (ktock/qemu-wasm builds qemu-system-aarch64.wasm; the demo-images repo
#  ships the prebuilt qemu-system-aarch64.{wasm,data,worker.js}, out.js and
#  load.js plus the kernel8.img / DTB / busybox rootfs packed into .data.)
set -eu

DEST="public/linux"
mkdir -p "$DEST"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

echo "Cloning ktock/qemu-wasm-demo-images (sparse: raspi3ap) ..."
git clone --depth 1 --filter=blob:none --sparse \
    https://github.com/ktock/qemu-wasm-demo-images.git "$TMP"
( cd "$TMP" && git sparse-checkout set raspi3ap )

for f in qemu-system-aarch64.wasm qemu-system-aarch64.data \
         qemu-system-aarch64.worker.js out.js load.js; do
    cp "$TMP/raspi3ap/$f" "$DEST/$f"
    echo "  -> $DEST/$f ($(wc -c < "$DEST/$f") bytes)"
done

echo "Done. The 'linux' boot mode is ready."
