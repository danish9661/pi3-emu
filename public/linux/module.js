if (typeof Module === 'undefined') {
    Module = {};
}

// Read boot config from parent window (same-origin iframe). The main page
// sets window.__linuxConfig before loading this iframe. Falls back to
// URL hash or 'minimal'.
let bootConfig = 'minimal';
try {
  if (window.parent && window.parent !== window && window.parent.__linuxConfig) {
    bootConfig = window.parent.__linuxConfig;
  }
} catch (_) {}
if (bootConfig === 'minimal' && location.hash) {
  bootConfig = location.hash.replace('#', '') || 'minimal';
}

const KERNEL_COMMON = 'console=ttyAMA0,115200 lpj=7000000 nokaslr mitigations=off nowatchdog nosoftlockup audit=0 cgroup_disable=memory ipv6.disable=1 cryptomgr.notests';

const BLACKLISTS = {
  // minimal: fastest boot, USB fully blacklisted (OTG PHY not needed)
  minimal: 'initcall_blacklist=bcm2835_pm_driver_init,bcm2835_cpufreq_init,bcm2835_wdt_init,leds-gpio,thermal,gpio-fan,pwm-fan,dwc2,xhci-hcd,smsc95xx,usb_ernet,rndis_host,cdc_ether,usb-storage,sdhci-iproc,i2c-bcm2835,spi-bcm2835,bcm2835-rng,brcmstb_thermal,snd_bcm2835,vchiq,snd_pcm,snd_timer,snd,soundcore,joydev,rfkill,bcm2835_v4l2,cfg80211,rfkill_gpio',
  // standard: balanced, USB still blacklisted (needs OTG PHY, slow)
  standard: 'initcall_blacklist=bcm2835_pm_driver_init,bcm2835_cpufreq_init,bcm2835_wdt_init,thermal,gpio-fan,pwm-fan,dwc2,xhci-hcd,smsc95xx,usb_ernet,rndis_host,cdc_ether,usb-storage,brcmstb_thermal,snd_bcm2835,vchiq,joydev,rfkill,bcm2835_v4l2,cfg80211,rfkill_gpio',
  // usb: enables dwc2 host stack (needs our OTG PHY in src/usb.js / QEMU dwc2). Slower (~+5-8s), enables downstream usb-storage/ernet.
  usb: 'initcall_blacklist=bcm2835_pm_driver_init,bcm2835_cpufreq_init,leds-gpio,thermal,gpio-fan,pwm-fan,smsc95xx,usb_ernet,rndis_host,cdc_ether,usb-storage,sdhci-iproc,i2c-bcm2835,spi-bcm2835,bcm2835-rng,brcmstb_thermal,snd_bcm2835,vchiq,joydev,rfkill,bcm2835_v4l2,cfg80211,rfkill_gpio',
  // full: all drivers (includes dwc2 + usb-storage + xhci). Slowest, most complete. OTG PHY must handle VBUS/session.
  full: 'initcall_blacklist=bcm2835_pm_driver_init,bcm2835_cpufreq_init,thermal,gpio-fan,pwm-fan,brcmstb_thermal,snd_bcm2835,vchiq,joydev,rfkill,bcm2835_v4l2,cfg80211,rfkill_gpio',
  custom: 'initcall_blacklist=bcm2835_pm_driver_init,bcm2835_cpufreq_init,bcm2835_wdt_init,thermal,gpio-fan,pwm-fan,dwc2,xhci-hcd,smsc95xx,usb_ernet,rndis_host,cdc_ether,usb-storage,sdhci-iproc,brcmstb_thermal,snd_bcm2835,vchiq,joydev,rfkill,bcm2835_v4l2,cfg80211,rfkill_gpio',
};

const LOGLEVEL = bootConfig === 'full' ? 'loglevel=5' : 'loglevel=1';

Module['arguments'] = [
    '-nic', 'none',
    '-M', 'raspi3ap', '-nographic',
    '-m', '512M',
    '-accel', 'tcg,tb-size=500,thread=multi', '-smp', '4,sockets=4',
    '-dtb', '/pack/bcm2710-rpi-3-b-plus.dtb',
    '-kernel', '/pack/kernel8.img',
    '-drive', 'file=/pack/rootfs.bin,format=raw,if=sd',
    '-append', KERNEL_COMMON + ' root=/dev/mmcblk0 rootwait ' + LOGLEVEL + ' ' + (BLACKLISTS[bootConfig] || BLACKLISTS.minimal)
];

(function () {
    const here = (document.currentScript && document.currentScript.src) || location.href;
    const dir = here.replace(/[^\/]*$/, '');
    Module['locateFile'] = function (path, prefix) { return dir + path; };
    Module['mainScriptUrlOrBlob'] = dir + 'out.js';
})();
