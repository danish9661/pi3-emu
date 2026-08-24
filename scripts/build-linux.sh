#!/usr/bin/env bash
#
# scripts/build-linux.sh — build the qemu-wasm raspi3ap Linux engine from source.
#
# Produces the same artifacts the prebuilt ktock/qemu-wasm-demo-images ships
# (qemu-system-aarch64.wasm/.data/.worker.js, out.js, load.js) into public/linux/.
# Faithful to ktock/qemu-wasm's README. Requires podman (or docker via $DOCKER).
#
# The build is LONG (emscripten compiles QEMU to wasm — often 30+ min). It is
# RESUMABLE: the build container is kept between runs (its /build tree persists),
# so `emmake make` continues from existing object files if a previous run was
# interrupted. Just re-run this script to continue.
#
# Env overrides:
#   DOCKER=podman          container engine (default: podman)
#   QEMU_WASM_SRC=<dir>   existing ktock/qemu-wasm checkout (default: clones a
#                          shallow copy into scripts/.build/qemu-wasm)
#   QEMU_WASM_REF=HEAD     ref/branch/tag to check out
set -euo pipefail

DOCKER="${DOCKER:-podman}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LINUX_DIR="$ROOT/public/linux"
SRC="${QEMU_WASM_SRC:-}"
REF="${QEMU_WASM_REF:-HEAD}"
IMG="buildqemu"
CTN="build-qemu-wasm"

mkdir -p "$LINUX_DIR"

if [ -z "$SRC" ]; then
  SRC="$ROOT/scripts/.build/qemu-wasm"
  if [ ! -d "$SRC/.git" ]; then
    echo "[*] cloning ktock/qemu-wasm ($REF) into $SRC"
    git clone --depth 1 "${QEMU_WASM_REF:+--branch $QEMU_WASM_REF}" \
      https://github.com/ktock/qemu-wasm "$SRC"
  fi
fi
echo "[*] qemu-wasm source: $SRC"

# 1) base image (emscripten + zlib/libffi/glib/pixman + xterm-pty) — cached
if ! $DOCKER image exists "$IMG" 2>/dev/null; then
  echo "[*] building base image $IMG (slow: pulls emscripten, builds glib/pixman)"
  $DOCKER build -t "$IMG" - < "$SRC/Dockerfile"
else
  echo "[*] base image $IMG already present, reusing"
fi

# 2) build container — reused across runs so /build (object files) persists
if ! $DOCKER ps -a --format '{{.Names}}' | grep -qx "$CTN"; then
  echo "[*] creating build container $CTN"
  $DOCKER run --name "$CTN" -d "$IMG" sleep infinity
fi
$DOCKER start "$CTN" >/dev/null 2>&1 || true

# 3) copy the source into the container fs once (host bind mounts can be
#    noexec under rootless podman, which breaks executing /qemu/configure)
if ! $DOCKER exec "$CTN" test -f /qemu/configure; then
  echo "[*] copying source into container at /qemu"
  $DOCKER cp "$SRC"/. "$CTN:/qemu"
else
  echo "[*] source already in container, reusing"
fi

# 4) configure once
if ! $DOCKER exec "$CTN" test -f /build/config-host.mak; then
  echo "[*] configuring qemu (aarch64-softmmu, wasm32)"
  $DOCKER exec -it "$CTN" emconfigure /qemu/configure --static \
    --target-list=aarch64-softmmu --cpu=wasm32 --cross-prefix=
else
  echo "[*] already configured, skipping configure"
fi

# 5) build qemu-system-aarch64 — RESUMABLE across invocations
echo "[*] building qemu-system-aarch64 (emscripten -> wasm; resumes if interrupted)"
$DOCKER exec -it "$CTN" emmake make -j "$(nproc)" qemu-system-aarch64

# 6) build the kernel + dtb + busybox rootfs image
echo "[*] building raspi3ap guest image (kernel8.img + dtb + rootfs.bin)"
PACK="$(mktemp -d)"
$DOCKER build --output=type=local,dest="$PACK" "$SRC/examples/raspi3ap/image"
$DOCKER cp "$PACK/." "$CTN:/pack"
rm -rf "$PACK"

# 7) package the /pack directory into the .data preload bundle
echo "[*] packaging qemu-system-aarch64.data (preload /pack)"
$DOCKER exec -it "$CTN" /bin/sh -c \
  "/emsdk/upstream/emscripten/tools/file_packager.py qemu-system-aarch64.data --preload /pack > load.js"

# 8) copy artifacts into public/linux/
echo "[*] copying artifacts into $LINUX_DIR"
$DOCKER cp "$CTN:/build/qemu-system-aarch64" "$LINUX_DIR/out.js"
for f in qemu-system-aarch64.wasm qemu-system-aarch64.worker.js qemu-system-aarch64.data load.js; do
  $DOCKER cp "$CTN:/build/$f" "$LINUX_DIR/$f"
done

echo "[done] built artifacts in $LINUX_DIR — reload the linux boot option to use them."
