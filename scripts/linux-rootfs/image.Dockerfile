ARG BUSYBOX_VERSION=1.36.1
ARG KERNEL_TAG=1.20230405

FROM ubuntu:22.04 AS rootfs-dev
RUN apt-get update && apt-get install -y gcc gcc-aarch64-linux-gnu linux-libc-dev-arm64-cross git make fakeroot cpio
ARG BUSYBOX_VERSION
RUN apt-get update -y && apt-get install -y gcc bzip2 wget fakeroot libc6-dev-arm64-cross
WORKDIR /work
RUN wget https://busybox.net/downloads/busybox-${BUSYBOX_VERSION}.tar.bz2
RUN bzip2 -d busybox-${BUSYBOX_VERSION}.tar.bz2
RUN tar xvf busybox-${BUSYBOX_VERSION}.tar
WORKDIR /work/busybox-${BUSYBOX_VERSION}
RUN make CROSS_COMPILE=aarch64-linux-gnu- LDFLAGS=--static defconfig
RUN make CROSS_COMPILE=aarch64-linux-gnu- LDFLAGS=--static -j$(nproc)
RUN mkdir -p /rootfs/bin && mv busybox /rootfs/bin/busybox
RUN make LDFLAGS=--static defconfig
RUN make LDFLAGS=--static -j$(nproc)
RUN for i in $(./busybox --list) ; do ln -s busybox /rootfs/bin/$i ; done
RUN mkdir -p /rootfs/proc /rootfs/sys /rootfs/mnt /rootfs/run /rootfs/tmp /rootfs/dev /rootfs/var /rootfs/etc /rootfs/lib /rootfs/usr
COPY ./rcS /rootfs/etc/init.d/
RUN chmod 700 /rootfs/etc/init.d/rcS
COPY ./hostname /rootfs/etc/hostname
COPY ./motd /rootfs/etc/motd
COPY ./profile /rootfs/etc/profile
COPY ./hw /rootfs/bin/hw
RUN chmod 755 /rootfs/bin/hw
COPY ./bridge-demo.sh /rootfs/bin/bridge-demo
RUN chmod 755 /rootfs/bin/bridge-demo
COPY ./bridge-demo.c /rootfs/root/bridge-demo.c
COPY ./inittab /rootfs/etc/inittab
COPY ./passwd /rootfs/etc/passwd
RUN chmod 644 /rootfs/etc/passwd
COPY ./shadow /rootfs/etc/shadow
RUN chmod 600 /rootfs/etc/shadow
RUN mkdir -p /rootfs/mnt/incoming
# ---- glibc (shared loader + libs) + C headers for a real dev environment ----
RUN cp -a /usr/aarch64-linux-gnu/lib/. /rootfs/lib/
RUN mkdir -p /rootfs/usr/include && cp -a /usr/aarch64-linux-gnu/include/. /rootfs/usr/include/
# ---- build tcc (static) + libtcc1.a ----
RUN git clone --depth 1 https://repo.or.cz/tinycc.git /tcc
WORKDIR /tcc
RUN gcc -DC2STR -o c2str.exe conftest.c && touch c2str.exe
RUN ./configure --cross-prefix=aarch64-linux-gnu- --cpu=arm64 --enable-static
RUN make 2>&1 | tail -5 || true
RUN aarch64-linux-gnu-gcc -c lib/lib-arm64.c -o /tmp/lib-arm64.o -I lib -I include && \
    aarch64-linux-gnu-ar rcs /tmp/libtcc1.a /tmp/lib-arm64.o
RUN cp -f tcc /rootfs/bin/tcc && chmod 755 /rootfs/bin/tcc && \
    mkdir -p /rootfs/lib/tcc /rootfs/usr/lib/tcc && \
    cp -f /tmp/libtcc1.a /rootfs/lib/tcc/libtcc1.a && \
    cp -f /tmp/libtcc1.a /rootfs/usr/lib/tcc/libtcc1.a
# Build a gzipped-cpio initramfs (the live engine boots it via -initrd /pack/rootfs.bin).
# Run as root so the cpio preserves uid 0 (root) in the guest. /init mounts the
# devtmpfs/proc/sys filesystems and hands off to busybox init (which reads /etc/inittab).
# /dev is excluded — the guest mounts devtmpfs over it at boot.
RUN printf '#!/bin/sh\nmount -t devtmpfs none /dev 2>/dev/null || true\nmount -t proc none /proc 2>/dev/null || true\nmount -t sysfs none /sys 2>/dev/null || true\nexec /bin/init\n' > /rootfs/init && chmod 755 /rootfs/init
RUN mkdir -p /out && cd /rootfs && find . -path './dev' -prune -o -print0 | cpio --null -o -H newc | gzip -9 > /out/rootfs.bin
# Fail loudly on an empty archive: without `cpio` installed (or a broken
# find pipe) gzip still exits 0, producing a ~20-byte rootfs that only dies
# later at guest VFS-mount time. A real dev rootfs is tens of MB.
RUN test $(stat -c%s /out/rootfs.bin) -gt 1000000

FROM ubuntu:24.04 AS kernel-dev
ARG KERNEL_TAG
WORKDIR /work
RUN apt-get update && apt-get install -y crossbuild-essential-arm64 git gperf bc bison flex libssl-dev make libc6-dev libncurses5-dev wget
RUN wget https://github.com/raspberrypi/linux/archive/refs/tags/${KERNEL_TAG}.tar.gz
RUN tar -xzvf ${KERNEL_TAG}.tar.gz
WORKDIR /work/linux-${KERNEL_TAG}
RUN ls
RUN make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- bcm2711_defconfig
RUN make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu- -j$(nproc) Image dtbs
RUN mkdir /out
RUN cp arch/arm64/boot/Image /out/kernel8.img
RUN cp arch/arm64/boot/dts/broadcom/bcm2710-rpi-3-b-plus.dtb /out/

FROM scratch
COPY --from=rootfs-dev /out/rootfs.bin /
COPY --from=kernel-dev /out/ /
