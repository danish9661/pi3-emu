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
#   THREADS=mt|st          threading target (default: mt). Also settable as the
#                          first CLI arg: scripts/build-linux.sh --threads=st.
#                          mt = pthread/MTTCG engine (needs SharedArrayBuffer,
#                          installs to public/linux). st = single-thread engine
#                          (no pthreads, runs without cross-origin isolation,
#                          installs to public/linux-st); the harness
#                          auto-selects it when SAB is unavailable.
set -euo pipefail

THREADS="${THREADS:-mt}"
for _a in "$@"; do
  case "$_a" in
    --threads=mt) THREADS="mt" ;;
    --threads=st) THREADS="st" ;;
    --threads=*) echo "unknown $_a (want --threads=mt|st)" >&2; exit 1 ;;
  esac
done
if [ "$THREADS" != "mt" ] && [ "$THREADS" != "st" ]; then
  echo "THREADS must be mt|st (got $THREADS)" >&2; exit 1
fi
echo "[*] threading target: $THREADS"

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
# mt: ktock exact flags incl. pthreads. st: same minus proxy/pthread support,
# so the engine instantiates a plain (non-shared) WebAssembly.Memory; QEMU
# runs thread=single (one RR vCPU thread, backend init via apply-st-patches).
# Goal: boot anywhere (iOS, file://, no COOP/COEP) — slower, but universal.
if [ "$THREADS" = "st" ]; then
  EXTRA_CFLAGS="-O3 -g -Wno-error=unused-command-line-argument -mbulk-memory -DNDEBUG -DG_DISABLE_ASSERT -D_GNU_SOURCE -sASYNCIFY=1 -sFORCE_FILESYSTEM -sALLOW_TABLE_GROWTH -sTOTAL_MEMORY=2300MB -sWASM_BIGINT -sMALLOC=mimalloc --js-library=$PTY_LIB -sEXPORT_ES6=1 -sASYNCIFY_IMPORTS=ffi_call_js"
else
  EXTRA_CFLAGS="-O3 -g -Wno-error=unused-command-line-argument -matomics -mbulk-memory -DNDEBUG -DG_DISABLE_ASSERT -D_GNU_SOURCE -sASYNCIFY=1 -pthread -sPROXY_TO_PTHREAD=1 -sFORCE_FILESYSTEM -sALLOW_TABLE_GROWTH -sTOTAL_MEMORY=2300MB -sWASM_BIGINT -sMALLOC=mimalloc --js-library=$PTY_LIB -sEXPORT_ES6=1 -sASYNCIFY_IMPORTS=ffi_call_js"
fi
EXTRA_LDFLAGS="$EXTRA_CFLAGS -L/build/target/lib -sEXPORTED_RUNTIME_METHODS=getTempRet0,setTempRet0,addFunction,removeFunction,TTY,FS,ccall"
if [ "$THREADS" = "st" ]; then
  # No worker pre-spawn: the default pool (4 workers x private 2.3 GB heaps =
  # 9 GB+ and instant incoherence without shared memory) must never start.
  # The single RR vCPU thread is spawned on demand by QEMU itself.
  EXTRA_LDFLAGS="$EXTRA_LDFLAGS -sPTHREAD_POOL_SIZE=0"
fi

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

# 3b) inject the N4 pi3-ctl device source + header, and the PWM/SPI/I2C bridge
#     devices, register them in meson, and apply the board/gpio wiring patches.
#     pi3-ctl is wired directly to the bcm2835_gpio input/output lines (no MMIO);
#     the bridge devices have MMIO at the BCM2835 peripheral addresses.
#     Snapshot bridge (true VM savevm) is a stub MMIO at 0x3F300800.
$DOCKER cp "$ROOT/scripts/linux-rootfs/pi3ctl.c"  "$CTN:/qemu/hw/misc/pi3ctl.c"
$DOCKER cp "$ROOT/scripts/linux-rootfs/pi3ctl.h"  "$CTN:/qemu/include/hw/misc/pi3ctl.h"
$DOCKER cp "$ROOT/scripts/linux-rootfs/pwm-bridge.c" "$CTN:/qemu/hw/misc/pwm-bridge.c"
$DOCKER cp "$ROOT/scripts/linux-rootfs/pwm-bridge.h" "$CTN:/qemu/include/hw/misc/pwm-bridge.h"
$DOCKER cp "$ROOT/scripts/linux-rootfs/spi-bridge.c" "$CTN:/qemu/hw/misc/spi-bridge.c"
$DOCKER cp "$ROOT/scripts/linux-rootfs/spi-bridge.h" "$CTN:/qemu/include/hw/misc/spi-bridge.h"
$DOCKER cp "$ROOT/scripts/linux-rootfs/i2c-bridge.c" "$CTN:/qemu/hw/misc/i2c-bridge.c"
$DOCKER cp "$ROOT/scripts/linux-rootfs/i2c-bridge.h" "$CTN:/qemu/include/hw/misc/i2c-bridge.h"
$DOCKER cp "$ROOT/scripts/linux-rootfs/snapshot-bridge.c" "$CTN:/qemu/hw/misc/snapshot-bridge.c"
$DOCKER cp "$ROOT/scripts/linux-rootfs/snapshot-bridge.h" "$CTN:/qemu/include/hw/misc/snapshot-bridge.h"
$DOCKER cp "$ROOT/scripts/linux-rootfs/apply-n4-patches.py" "$CTN:/qemu/apply-n4-patches.py"
for f in pi3ctl pwm-bridge spi-bridge i2c-bridge snapshot-bridge; do
  $DOCKER exec "$CTN" bash -c "grep -q \"files('${f}.c')\" /qemu/hw/misc/meson.build || printf \"system_ss.add(files('${f}.c'))\n\" >> /qemu/hw/misc/meson.build"
done
echo "[*] applying N4 patches"
$DOCKER exec -w /qemu "$CTN" python3 /qemu/apply-n4-patches.py

# 3c) ST-only: backend init on the single-thread (RR) path. ktock's wasm32 JIT
#     installs Module.__wasm32_tb only in mttcg_cpu_thread_fn; without this the
#     first hot TB on thread=single dies in instantiate_wasm(). Idempotent.
if [ "$THREADS" = "st" ]; then
  $DOCKER cp "$ROOT/scripts/linux-rootfs/apply-st-patches.py" "$CTN:/qemu/apply-st-patches.py"
  echo "[*] applying ST patches"
  $DOCKER exec -w /qemu "$CTN" python3 /qemu/apply-st-patches.py
fi

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

# 8) install the rebuilt engine + initramfs bundle.
#    mt installs into the LIVE public/linux/ (pthread/MTTCG + pi3-ctl).
#    st installs into public/linux-st/ (single-thread fallback the harness
#    auto-selects when SharedArrayBuffer is unavailable); copy the committed
#    harness (index.html/module.js/load.js/vendor) alongside on first build so
#    the directory is bootable, then repoint its module.js accel flag via the
#    harness auto-detect (no manual edit needed).
if [ "$THREADS" = "st" ]; then
  BUILT_DIR="$ROOT/public/linux-st"
else
  BUILT_DIR="$ROOT/public/linux"
fi
mkdir -p "$BUILT_DIR"
echo "[*] installing rebuilt $THREADS engine + initramfs bundle into $BUILT_DIR"
$DOCKER cp "$CTN:$BUILD/qemu-system-aarch64" "$BUILT_DIR/out.js"
for f in qemu-system-aarch64.wasm qemu-system-aarch64.worker.js qemu-system-aarch64.data load.js; do
  $DOCKER cp "$CTN:$BUILD/$f" "$BUILT_DIR/$f"
done
if [ "$THREADS" = "st" ]; then
  # Seed the fallback dir with the committed harness so it boots standalone;
  # module.js auto-detects threads=off there (no SAB needed by the engine).
  # Clear first: re-running the seed over an existing dir would nest
  # vendor/ inside vendor/.
  rm -rf "$BUILT_DIR/vendor" "$BUILT_DIR/index.html" "$BUILT_DIR/module.js"
  for f in index.html module.js vendor; do
    [ -e "$ROOT/public/linux/$f" ] && cp -r "$ROOT/public/linux/$f" "$BUILT_DIR/$f"
  done
  # Deliberately do NOT create $BUILT_DIR/.bootable: the harness only routes
  # threads=off to linux-st/ when that sentinel exists, and it means "a shell
  # boot was verified here". Create it (e.g. `: > public/linux-st/.bootable`)
  # only after test/linux-boot-bench.mjs threads=off reports a shell.
  echo "[note] single-thread fallback ready in $BUILT_DIR (harness auto-selects it without SAB)"
fi

echo "[done] rebuilt $THREADS engine + initramfs installed in $BUILT_DIR"
