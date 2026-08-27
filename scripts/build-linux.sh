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
# ktock/qemu-wasm exact build flags (README, aarch64) + ccall export for pi3_rx.
# Replicate the documented command as closely as possible. The -s flags are
# passed via --extra-cflags AND --extra-ldflags so they reach both compile and
# link (qemu's final link only uses LDFLAGS).
PTY_LIB="/opt/xterm-pty/node_modules/xterm-pty/emscripten-pty.js"
EXTRA_CFLAGS="-O3 -g -Wno-error=unused-command-line-argument -matomics -mbulk-memory -DNDEBUG -DG_DISABLE_ASSERT -D_GNU_SOURCE -sASYNCIFY=1 -pthread -sPROXY_TO_PTHREAD=1 -sFORCE_FILESYSTEM -sALLOW_TABLE_GROWTH -sTOTAL_MEMORY=2300MB -sWASM_BIGINT -sMALLOC=mimalloc --js-library=$PTY_LIB -sEXPORT_ES6=1 -sASYNCIFY_IMPORTS=ffi_call_js"
EXTRA_LDFLAGS="$EXTRA_CFLAGS -L/build/target/lib -sEXPORTED_RUNTIME_METHODS=getTempRet0,setTempRet0,addFunction,removeFunction,TTY,FS,ccall"

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

# 2b) ensure the xterm-pty js-library is present (the TTY needs
#     --js-library=$PTY_LIB). Install it OUTSIDE /build so the step-4
#     `rm -rf /build` (when flags change) doesn't wipe it. Idempotent.
$DOCKER exec "$CTN" bash -c "test -f $PTY_LIB || (mkdir -p /opt/xterm-pty && cd /opt/xterm-pty && npm i xterm-pty@v0.10.1)" || true

# 3) copy the source into the container fs once (host bind mounts can be
#    noexec under rootless podman, which breaks executing /qemu/configure)
if ! $DOCKER exec "$CTN" test -f /qemu/configure; then
  echo "[*] copying source into container at /qemu"
  $DOCKER cp "$SRC"/. "$CTN:/qemu"
else
  echo "[*] source already in container, reusing"
fi

# 3b) inject the N4 pi3-ctl device source + header, register it in meson,
#     and apply the board/gpio wiring patches (no MMIO: pi3-ctl is wired
#     directly to the bcm2835_gpio input/output lines by hw/arm/raspi.c).
$DOCKER cp "$ROOT/scripts/linux-rootfs/pi3ctl.c"  "$CTN:/qemu/hw/misc/pi3ctl.c"
$DOCKER cp "$ROOT/scripts/linux-rootfs/pi3ctl.h"  "$CTN:/qemu/include/hw/misc/pi3ctl.h"
$DOCKER cp "$ROOT/scripts/linux-rootfs/apply-n4-patches.py" "$CTN:/qemu/apply-n4-patches.py"
$DOCKER exec "$CTN" bash -c "grep -q \"files('pi3ctl.c')\" /qemu/hw/misc/meson.build || printf \"system_ss.add(files('pi3ctl.c'))\n\" >> /qemu/hw/misc/meson.build"
echo "[*] applying N4 patches"
$DOCKER exec -w /qemu "$CTN" python3 /qemu/apply-n4-patches.py

# 4) configure. Build in /qb (NOT /build) so we never disturb the image-provided
#    /build/target sysroot (glib/zlib/pixman built for wasm32). Reuse /qb only if
#    it was configured with THESE exact flags; otherwise blow it away. The
#    signature is written at the end of a successful configure.
BUILD=/qb
FLAG_SIG=$(printf '%s|%s' "$EXTRA_CFLAGS" "$EXTRA_LDFLAGS" | sha256sum | cut -d' ' -f1)
if ! $DOCKER exec -e FLAG_SIG="$FLAG_SIG" "$CTN" sh -c "test -f $BUILD/.n4sig && [ \"\$(cat $BUILD/.n4sig)\" = '$FLAG_SIG' ]" 2>/dev/null; then
  echo "[*] flags changed or no valid config — cleaning $BUILD"
  $DOCKER exec "$CTN" rm -rf "$BUILD"
fi
if ! $DOCKER exec "$CTN" test -f "$BUILD/config-host.mak"; then
  CONF="mkdir -p $BUILD && cd $BUILD && emconfigure /qemu/configure --static --disable-werror --target-list=aarch64-softmmu --cpu=wasm32 --cross-prefix= --extra-cflags='$EXTRA_CFLAGS' --extra-cxxflags='$EXTRA_CFLAGS' --extra-ldflags='$EXTRA_LDFLAGS'"
  echo "[*] configuring qemu (aarch64-softmmu, wasm32) — pass 1 (downloads dtc subproject)"
  $DOCKER exec -it -e EXTRA_CFLAGS="$EXTRA_CFLAGS" -e EXTRA_LDFLAGS="$EXTRA_LDFLAGS" "$CTN" bash -c "$CONF"
  # dtc is a meson wrap downloaded during meson setup; its meson.build sets
  # default_options: 'werror=true', which turns emscripten's unused-arg
  # warnings (e.g. -no-pie) into hard errors. Patch it, then reconfigure.
  $DOCKER exec "$CTN" bash -c 'test -f /qemu/subprojects/dtc/meson.build && sed -i "s/werror=true/werror=false/" /qemu/subprojects/dtc/meson.build && echo "[*] dtc werror disabled"'
  echo "[*] configuring qemu — pass 2 (applies patched dtc)"
  $DOCKER exec -it -e EXTRA_CFLAGS="$EXTRA_CFLAGS" -e EXTRA_LDFLAGS="$EXTRA_LDFLAGS" "$CTN" bash -c "$CONF"
  $DOCKER exec -e FLAG_SIG="$FLAG_SIG" "$CTN" bash -c "echo '$FLAG_SIG' > $BUILD/.n4sig"
else
  echo "[*] config matches flags, reusing $BUILD (resumable)"
fi

# 5) build qemu-system-aarch64 — RESUMABLE across invocations
echo "[*] building qemu-system-aarch64 (emscripten -> wasm; resumes if interrupted)"
$DOCKER exec -it -e EXTRA_CFLAGS="$EXTRA_CFLAGS" -e EXTRA_LDFLAGS="$EXTRA_LDFLAGS" "$CTN" \
  bash -c "cd $BUILD && emmake make -j \"\$(nproc)\" qemu-system-aarch64"

# 6) build the kernel + dtb + rootfs image (rootfs carries busybox + glibc +
#    C headers + tcc, per scripts/linux-rootfs/image.Dockerfile). The build
#    context is scripts/linux-rootfs so the committed rcS/inittab/passwd/...
#    sources are used (not the upstream ones from the clone).
echo "[*] building raspi3ap guest image (kernel8.img + dtb + rootfs.bin)"
ROOTFS_CTX="$ROOT/scripts/linux-rootfs"
cp "$ROOTFS_CTX/image.Dockerfile" "$SRC/examples/raspi3ap/image/Dockerfile"
PACK="$(mktemp -d)"
$DOCKER build --output=type=local,dest="$PACK" -f "$ROOTFS_CTX/image.Dockerfile" "$ROOTFS_CTX"
$DOCKER cp "$PACK/." "$CTN:/pack"
rm -rf "$PACK"

# 7) package the /pack directory into the .data preload bundle (run in /build)
echo "[*] packaging qemu-system-aarch64.data (preload /pack)"
$DOCKER exec -it -w "$BUILD" "$CTN" /bin/sh -c \
  "/emsdk/upstream/emscripten/tools/file_packager.py qemu-system-aarch64.data --preload /pack > load.js"

# 8) install the rebuilt engine + initramfs bundle into the LIVE public/linux/.
#    The .data (dtb + kernel + gzipped-cpio rootfs) and load.js are rebuilt as
#    part of this pipeline (steps 6/7) so the deploy is self-consistent with
#    module.js' -initrd boot. NOTE: this is a from-source build, so the kernel is
#    ktock's upstream bcm2711_defconfig kernel (slower than the raspi prebuilt
#    fast kernel). To keep the fast prebuilt kernel, re-stitch it after this step
#    using the manual repack recipe in AGENTS.md (M27): the .data layout is
#    dtb[0:32753] | kernel[32753:22505969] | rootfs[22505969:end], and both the
#    repacked .data and load.js must be updated together.
BUILT_DIR="$ROOT/public/linux"
mkdir -p "$BUILT_DIR"
echo "[*] installing rebuilt engine + initramfs bundle into LIVE $BUILT_DIR"
$DOCKER cp "$CTN:$BUILD/qemu-system-aarch64" "$BUILT_DIR/out.js"
for f in qemu-system-aarch64.wasm qemu-system-aarch64.worker.js qemu-system-aarch64.data load.js; do
  $DOCKER cp "$CTN:$BUILD/$f" "$BUILT_DIR/$f"
done

echo "[done] rebuilt live engine (pthread/MTTCG + pi3-ctl) + initramfs installed in $BUILT_DIR"
