if (typeof Module === 'undefined') {
    Module = {};
}

// Read boot config + threading mode from the parent window (same-origin
// iframe). The main page sets window.__linuxConfig/__linuxThreads before
// loading this iframe. Falls back to URL query (?cfg=&threads=), then URL
// hash (#cfg=..&threads=.. or legacy bare "#minimal").
let bootConfig = 'minimal';
let threadsRequested = 'auto'; // auto | on | off
let threadsFromDefault = true; // set false by any explicit source below
try {
  if (window.parent && window.parent !== window) {
    if (window.parent.__linuxConfig) bootConfig = window.parent.__linuxConfig;
    if (window.parent.__linuxThreads) {
      threadsRequested = window.parent.__linuxThreads;
      threadsFromDefault = false;
    }
  }
} catch (_) {}
try {
  const q = new URLSearchParams(location.search || '');
  if (q.get('cfg')) bootConfig = q.get('cfg');
  if (q.get('threads')) { threadsRequested = q.get('threads'); threadsFromDefault = false; }
} catch (_) {}
if (location.hash) {
  const h = location.hash.replace(/^#/, '');
  if (h && h.indexOf('=') === -1) {
    if (bootConfig === 'minimal') bootConfig = h; // legacy bare "#minimal"
  } else {
    try {
      const hp = new URLSearchParams(h);
      if (hp.get('cfg')) bootConfig = hp.get('cfg');
      if (hp.get('threads')) { threadsRequested = hp.get('threads'); threadsFromDefault = false; }
    } catch (_) {}
  }
}
if (['auto', 'on', 'off'].indexOf(threadsRequested) === -1) threadsRequested = 'auto';
// Direct loads with no explicit source honor the stored user pick (set by
// SabToggle.bindSelect/setPreference on any page using this library).
if (threadsFromDefault) {
  try {
    const SAB0 = (typeof window !== 'undefined' && window.SabToggle) || null;
    if (SAB0) threadsRequested = SAB0.getPreference();
    else {
      const s = window.localStorage && window.localStorage.getItem('sab.threads');
      if (s === 'on' || s === 'off' || s === 'auto') threadsRequested = s;
    }
  } catch (_) {}
}

// Capability decision via the reusable SabToggle (public/sab-toggle.js,
// loaded before this file). Inline fallback keeps this file bootable if
// copied without it (assumes multi — the pre-toggle behavior).
var __sab = (typeof window !== 'undefined' && window.SabToggle) || null;
function __decideFallback(requested) {
  var sab = false;
  try {
    sab = !!window.crossOriginIsolated && typeof SharedArrayBuffer !== 'undefined';
    new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
  } catch (_) { sab = false; }
  return { requested: requested, effective: requested === 'off' ? 'single'
    : requested === 'on' ? 'multi' : (sab ? 'multi' : 'single'), sab: sab };
}
// Effective qemu TCG thread mode. "auto" follows SAB availability so the page
// never dies with out.js "bad memory" on platforms without isolation
// (iOS Safari, file://, VPNs stripping COOP/COEP…). The pthread engine can
// only boot thread=multi (its TCG→Wasm backend hangs with thread=single),
// so an effective "single" is a routing signal: index.html hands off to
// public/linux-st/ when built, else stops with a build panel (see
// __pi3NeedSt below) instead of starting a doomed boot.
var __mode = __sab ? __sab.decide(threadsRequested) : __decideFallback(threadsRequested);
var threadsEffective = __mode.effective;
var sabOn = __mode.sab;
try {
  window.__pi3ThreadsRequested = threadsRequested;
  window.__pi3ThreadsEffective = threadsEffective;
  window.__pi3Sab = sabOn;
  // True when this boot needs the dedicated single-thread engine: the
  // pthread build's TCG→Wasm backend requires MTTCG (thread=single hangs in
  // worker init with `tb_ptr_ptr` undefined), so "single" can only boot from
  // public/linux-st/ (scripts/build-linux.sh --threads=st). The harness in
  // index.html redirects there when it exists, else shows a build panel.
  window.__pi3IsStEngine = /linux-st\//.test(location.pathname || '');
  window.__pi3NeedSt = threadsEffective === 'single' && !window.__pi3IsStEngine;
} catch (_) {}

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

// Engine-aware boot media. The MT engine (public/linux/) boots an ext2 SD
// image (root=/dev/mmcblk0). The ST rebuild (public/linux-st/, from
// scripts/linux-rootfs/image.Dockerfile) emits the rootfs as a gzipped-cpio
// INITRAMFS, which must be handed to the kernel via -initrd — mounting it
// as an SD card would VFS-panic. Same kernel cmdline otherwise.
const IS_ST_ENGINE = !!window.__pi3IsStEngine;
Module['arguments'] = IS_ST_ENGINE ? [
    '-nic', 'none',
    '-M', 'raspi3ap', '-nographic',
    '-m', '512M',
    '-accel', 'tcg,tb-size=500,thread=' + threadsEffective, '-smp', '4,sockets=4',
    '-dtb', '/pack/bcm2710-rpi-3-b-plus.dtb',
    '-kernel', '/pack/kernel8.img',
    '-initrd', '/pack/rootfs.bin',
    '-append', KERNEL_COMMON + ' ' + LOGLEVEL + ' ' + (BLACKLISTS[bootConfig] || BLACKLISTS.minimal)
] : [
    '-nic', 'none',
    '-M', 'raspi3ap', '-nographic',
    '-m', '512M',
    '-accel', 'tcg,tb-size=500,thread=' + threadsEffective, '-smp', '4,sockets=4',
    '-dtb', '/pack/bcm2710-rpi-3-b-plus.dtb',
    '-kernel', '/pack/kernel8.img',
    '-drive', 'file=/pack/rootfs.bin,format=raw,if=sd',
    '-append', KERNEL_COMMON + ' root=/dev/mmcblk0 rootwait ' + LOGLEVEL + ' ' + (BLACKLISTS[bootConfig] || BLACKLISTS.minimal)
];
// Snapshot for the harness/tests: effective qemu argv without scraping.
Module['pi3Args'] = Module['arguments'].slice();

(function () {
    const here = (document.currentScript && document.currentScript.src) || location.href;
    const dir = here.replace(/[^\/]*$/, '');
    Module['locateFile'] = function (path, prefix) { return dir + path; };
    Module['mainScriptUrlOrBlob'] = dir + 'out.js';
})();
