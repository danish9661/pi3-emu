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

# ktock's Dockerfile bakes -sWASM_BIGINT into CFLAGS, but that is a *linker*
# setting — emscripten errors on it during compilation
# (-Werror=unused-command-line-argument). glib's meson wrap neutralizes that
# warning, but qemu's build does not, so we override CFLAGS/LDFLAGS for the
# qemu configure/make steps: drop -sWASM_BIGINT from compile flags (keep the
# -DWASM_BIGINT macro) and move it to LDFLAGS, plus a safety -Wno-error.
# ktock's Dockerfile bakes several -sX LINKER settings into CFLAGS, but those
# are link-time only — emscripten errors on them during compilation
# (-Werror=unused-command-line-argument). glib's meson wrap neutralizes that
# warning, but qemu's build does not. So we split: CFLAGS keeps only
# compile-safe flags; the -sX linker settings go in LDFLAGS. (-DWASM_BIGINT is
# the C macro and stays in CFLAGS; -sWASM_BIGINT is the link flag.)
QEMU_CFLAGS="-O2 -matomics -mbulk-memory -DNDEBUG -DWASM_BIGINT -pthread -Wno-error=unused-command-line-argument -Wno-error=implicit-function-declaration"
QEMU_LDFLAGS="-L/build/target/lib -O2 -matomics -mbulk-memory -pthread -sWASM_BIGINT -sMALLOC=mimalloc -sASYNCIFY=1 -sFORCE_FILESYSTEM"

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

# 2) build container — reused across runs so /build (object files) persists.
#    WORKDIR is /root (never /build) so `rm -rf /build` can't break later execs.
if ! $DOCKER ps -a --format '{{.Names}}' | grep -qx "$CTN"; then
  echo "[*] creating build container $CTN"
  $DOCKER run --name "$CTN" --workdir /root -d "$IMG" sleep infinity
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

# 4) configure (clean slate if a previous run left a config with the old flags)
if $DOCKER exec "$CTN" sh -c 'test -f /build/build.ninja && grep -Eq "c_args.*-sWASM_BIGINT" /build/build.ninja' 2>/dev/null; then
  echo "[*] stale config (old CFLAGS) — cleaning /build"
  $DOCKER exec "$CTN" rm -rf /build
fi
if ! $DOCKER exec "$CTN" test -f /build/config-host.mak; then
  CONF='mkdir -p /build && cd /build && emconfigure /qemu/configure --static --disable-werror --target-list=aarch64-softmmu --cpu=wasm32 --cross-prefix='
  echo "[*] configuring qemu (aarch64-softmmu, wasm32) — pass 1 (downloads dtc subproject)"
  $DOCKER exec -it -e CFLAGS="$QEMU_CFLAGS" -e LDFLAGS="$QEMU_LDFLAGS" "$CTN" bash -c "$CONF"
  # dtc is a meson wrap downloaded during meson setup; its meson.build sets
  # default_options: 'werror=true', which turns emscripten's unused-arg
  # warnings (e.g. -no-pie) into hard errors. Patch it, then reconfigure.
  $DOCKER exec "$CTN" bash -c 'test -f /qemu/subprojects/dtc/meson.build && sed -i "s/werror=true/werror=false/" /qemu/subprojects/dtc/meson.build && echo "[*] dtc werror disabled"'
  echo "[*] configuring qemu — pass 2 (applies patched dtc)"
  $DOCKER exec -it -e CFLAGS="$QEMU_CFLAGS" -e LDFLAGS="$QEMU_LDFLAGS" "$CTN" bash -c "$CONF"
else
  echo "[*] already configured (good flags), skipping configure"
fi

# 5) build qemu-system-aarch64 — RESUMABLE across invocations
echo "[*] building qemu-system-aarch64 (emscripten -> wasm; resumes if interrupted)"
$DOCKER exec -it -e CFLAGS="$QEMU_CFLAGS" -e LDFLAGS="$QEMU_LDFLAGS" "$CTN" \
  bash -c 'cd /build && emmake make -j "$(nproc)" qemu-system-aarch64'

# 6) build the kernel + dtb + busybox rootfs image
echo "[*] building raspi3ap guest image (kernel8.img + dtb + rootfs.bin)"
PACK="$(mktemp -d)"
$DOCKER build --output=type=local,dest="$PACK" "$SRC/examples/raspi3ap/image"
$DOCKER cp "$PACK/." "$CTN:/pack"
rm -rf "$PACK"

# 7) package the /pack directory into the .data preload bundle (run in /build)
echo "[*] packaging qemu-system-aarch64.data (preload /pack)"
$DOCKER exec -it -w /build "$CTN" /bin/sh -c \
  "/emsdk/upstream/emscripten/tools/file_packager.py qemu-system-aarch64.data --preload /pack > load.js"

# 8) copy artifacts into a SEPARATE dir. NEVER overwrite the live
#    public/linux/ (which holds the verified prebuilt pthread/MTTCG binary).
#    This checkout lacks PROXY_TO_PTHREAD, so the from-source qemu is
#    single-thread and must not replace the prebuilt engine.
BUILT_DIR="$ROOT/public/linux-fromsrc"
mkdir -p "$BUILT_DIR"
echo "[*] copying artifacts into $BUILT_DIR (NOT the live public/linux/)"
$DOCKER cp "$CTN:/build/qemu-system-aarch64" "$BUILT_DIR/out.js"
for f in qemu-system-aarch64.wasm qemu-system-aarch64.worker.js qemu-system-aarch64.data load.js; do
  $DOCKER cp "$CTN:/build/$f" "$BUILT_DIR/$f"
done

echo "[done] built (single-thread) artifacts in $BUILT_DIR — the live public/linux/ is untouched."
