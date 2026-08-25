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
    '-accel', 'tcg,tb-size=500,thread=multi', '-smp', '1,sockets=1',
    '-dtb', '/pack/bcm2710-rpi-3-b-plus.dtb',
    '-kernel', '/pack/kernel8.img',
    '-drive', 'file=/pack/rootfs.bin,format=raw,if=sd',
    // Speed: drop earlycon (huge early-boot serial flood under TCG) and use
    // 'quiet' (suppresses kernel printk noise). Userspace /dev/console writes
    // (getty prompt, shell) still appear, so auto-activate keeps working.
    // -smp 1 avoids calibrate_delay on 3 extra vCPUs (a slow TCG path).
    '-append', 'console=ttyAMA0,115200 quiet initcall_blacklist=bcm2835_pm_driver_init root=/dev/mmcblk0 rootfstype=ext4 rootwait no_console_suspend'
];
(function () {
    const here = (document.currentScript && document.currentScript.src) || location.href;
    const dir = here.replace(/[^\/]*$/, '');
    Module['locateFile'] = function (path, prefix) { return dir + path; };
    Module['mainScriptUrlOrBlob'] = dir + 'out.js';
})();
