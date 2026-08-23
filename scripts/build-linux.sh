#!/usr/bin/env bash
#
# scripts/build-linux.sh — build the qemu-wasm raspi3ap Linux engine from source.
#
# Produces the same artifacts the prebuilt ktock/qemu-wasm-demo-images ships
# (qemu-system-aarch64.wasm/.data/.worker.js, out.js, load.js) into public/linux/.
# Faithful to ktock/qemu-wasm's README. Requires podman (or docker via $DOCKER).
#
# The build is LONG (emscripten compiles QEMU to wasm — plan for ~1-2h and a
# network connection). Run it in the background:
#   setsid bash scripts/build-linux.sh > /tmp/build-linux.log 2>&1 &
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

# 1) base image (emscripten + zlib/libffi/glib/pixman + xterm-pty)
if ! $DOCKER image exists "$IMG" 2>/dev/null; then
  echo "[*] building base image $IMG (this is slow: pulls emscripten, builds glib/pixman)"
  $DOCKER build -t "$IMG" - < "$SRC/Dockerfile"
else
  echo "[*] base image $IMG already present, reusing"
fi

# 2) long-running build container with the source mounted read-only
if $DOCKER ps -a --format '{{.Names}}' | grep -qx "$CTN"; then
  $DOCKER rm -f "$CTN" >/dev/null
fi
echo "[*] starting build container $CTN"
$DOCKER run --rm -d --name "$CTN" "$IMG" sleep infinity

cleanup() { $DOCKER rm -f "$CTN" >/dev/null 2>&1 || true; }
trap cleanup EXIT

# Bring the qemu-wasm source into the container's own filesystem. Host bind
# mounts can be noexec under rootless podman (breaks executing /qemu/configure).
if [ -n "$SRC" ]; then
  echo "[*] copying source into container at /qemu"
  $DOCKER cp "$SRC"/. "$CTN:/qemu"
else
  echo "[*] cloning ktock/qemu-wasm into container at /qemu"
  $DOCKER exec -it "$CTN" git clone --depth 1 https://github.com/ktock/qemu-wasm /qemu
fi

# 3) configure + make qemu-system-aarch64 (the long step)
echo "[*] configuring qemu (aarch64-softmmu, wasm32)"
$DOCKER exec -it "$CTN" emconfigure /qemu/configure --static \
  --target-list=aarch64-softmmu --cpu=wasm32 --cross-prefix=
echo "[*] building qemu-system-aarch64 (emscripten -> wasm; very long)"
$DOCKER exec -it "$CTN" emmake make -j "$(nproc)" qemu-system-aarch64

# 4) build the kernel + dtb + busybox rootfs image
echo "[*] building raspi3ap guest image (kernel8.img + dtb + rootfs.bin)"
PACK="$(mktemp -d)"
$DOCKER build --output=type=local,dest="$PACK" "$SRC/examples/raspi3ap/image"
$DOCKER cp "$PACK/." "$CTN:/pack"
rm -rf "$PACK"

# 5) package the /pack directory into the .data preload bundle
echo "[*] packaging qemu-system-aarch64.data (preload /pack)"
$DOCKER exec -it "$CTN" /bin/sh -c \
  "/emsdk/upstream/emscripten/tools/file_packager.py qemu-system-aarch64.data --preload /pack > load.js"

# 6) copy artifacts into public/linux/
echo "[*] copying artifacts into $LINUX_DIR"
$DOCKER cp "$CTN:/build/qemu-system-aarch64" "$LINUX_DIR/out.js"
for f in qemu-system-aarch64.wasm qemu-system-aarch64.worker.js qemu-system-aarch64.data load.js; do
  $DOCKER cp "$CTN:/build/$f" "$LINUX_DIR/$f"
done

echo "[done] built artifacts in $LINUX_DIR — reload the linux boot option to use them."
