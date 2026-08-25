if (typeof Module === 'undefined') {
    Module = {};
}
Module['arguments'] = [
    '-nic', 'none',
    '-M', 'raspi3ap', '-nographic',
    '-m', '512M',
    // MTTCG: JIT-compiled TBs run on worker threads. This is the working config
    // for ktock/qemu-wasm's pthread build — the vCPU threads keep the pthread
    // worker machinery warm (single-thread triggers a 'start is not a function'
    // worker race). Needs cross-origin isolation (COI service worker).
    '-accel', 'tcg,tb-size=500,thread=multi', '-smp', '4,sockets=4',
    '-dtb', '/pack/bcm2710-rpi-3-b-plus.dtb',
    '-kernel', '/pack/kernel8.img',
    // Root filesystem is an initramfs (gzipped cpio) loaded via -initrd instead
    // of an emulated SD card. This skips the slow/unreliable SD/MMC emulation
    // (the sdhci IRQ never fires under TCG -> 'mmc1 Timeout' noise) and is a
    // faster, more deterministic boot path. See /init in the cpio for the
    // boot entrypoint (mounts devtmpfs/proc/sys, hands off to busybox init).
    '-initrd', '/pack/rootfs.bin',
    // Speed: drop earlycon (huge early-boot serial flood under TCG) and use
    // 'quiet' (suppresses kernel printk noise). Userspace /dev/console writes
    // (getty prompt, shell) still appear, so auto-activate keeps working.
    // NOTE: -smp must stay 4 (raspi3ap enforces the SoC's 4 cores; -smp 1 is
    // rejected with "invalid smp cpu"). The initramfs root is the main speed win.
    '-append', 'console=ttyAMA0,115200 quiet initcall_blacklist=bcm2835_pm_driver_init no_console_suspend'
];
(function () {
    const here = (document.currentScript && document.currentScript.src) || location.href;
    const dir = here.replace(/[^\/]*$/, '');
    Module['locateFile'] = function (path, prefix) { return dir + path; };
    Module['mainScriptUrlOrBlob'] = dir + 'out.js';
})();
