/* SabToggle — reusable SharedArrayBuffer on/off switch for any project.
 *
 * Drop-in: no dependencies, no build step.
 *   <script src="./sab-toggle.js"></script>
 *   const mode = SabToggle.resolve(SabToggle.getPreference());
 *   // mode.requested: 'auto'|'on'|'off'   (what the user picked)
 *   // mode.effective: 'multi'|'single'    (what the app should boot)
 *   // mode.sab: true|false                (shared memory actually usable)
 *   // mode.reason: human-readable why
 *
 * The three modes:
 *   auto — multi-threaded engine when SharedArrayBuffer is usable, else the
 *          single-thread fallback. Never crashes: the right choice for links
 *          you share (iOS Safari, file://, VPNs stripping COOP/COEP…).
 *   on   — force the multi-threaded (pthread) engine. Fails loudly with a
 *          clear message where isolation is unavailable.
 *   off  — force the single-threaded engine. Needs an ST build next to the
 *          MT one (see pickVariant); without it you get a build panel,
 *          never a black screen.
 *
 * Preference order (first wins): explicit argument > ?threads= URL param >
 * localStorage ("sab.threads") > "auto". Use SabToggle.bindSelect(<select>)
 * for a one-line UI, or SabToggle.setPreference() from your own controls.
 *
 * Works as a classic script (window.SabToggle). Bundler projects can copy
 * this file (it has no imports) or load it with a plain <script> tag.
 */
(function (root) {
  "use strict";

  var KEY = "sab.threads";
  var VALID = { auto: 1, on: 1, off: 1 };

  function sanitize(v) {
    v = String(v == null ? "" : v).toLowerCase();
    return VALID[v] ? v : "auto";
  }

  // Full capability probe. Never throws: every check is guarded, including
  // the 1-page shared-memory allocation that proves the browser will grant
  // the real multi-GB one your engine wants.
  function detect() {
    var d = {
      secure: false, isolated: false, hasSAB: false, sharedWasmOk: false,
      supported: false, reason: "",
    };
    try {
      d.secure = typeof window !== "undefined" && !!window.isSecureContext;
      d.isolated = typeof window !== "undefined" && !!window.crossOriginIsolated;
      d.hasSAB = typeof SharedArrayBuffer !== "undefined";
      if (d.isolated && d.hasSAB &&
          typeof WebAssembly !== "undefined" && WebAssembly.Memory) {
        new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
        d.sharedWasmOk = true;
      }
    } catch (_) {
      d.sharedWasmOk = false;
    }
    d.supported = d.sharedWasmOk;
    d.reason = !d.secure ? "page is not a secure context (needs https or localhost)"
      : !d.isolated ? "page is not cross-origin isolated (needs COOP/COEP headers or the coi-serviceworker)"
      : !d.hasSAB ? "SharedArrayBuffer is undefined in this browser"
      : !d.sharedWasmOk ? "shared WebAssembly.Memory was refused"
      : "shared memory available";
    return d;
  }

  // Where the user's pick lives: explicit > ?threads= > localStorage > auto.
  function getPreference(explicit) {
    if (explicit != null && String(explicit) !== "") return sanitize(explicit);
    try {
      var q = new URLSearchParams(location.search || "").get("threads");
      if (q) return sanitize(q);
    } catch (_) {}
    try {
      var h = new URLSearchParams((location.hash || "").replace(/^#/, "")).get("threads");
      if (h) return sanitize(h);
    } catch (_) {}
    try {
      var s = root.localStorage && root.localStorage.getItem(KEY);
      if (s) return sanitize(s);
    } catch (_) {}
    return "auto";
  }

  function setPreference(v) {
    v = sanitize(v);
    try { if (root.localStorage) root.localStorage.setItem(KEY, v); } catch (_) {}
    try {
      root.dispatchEvent(new CustomEvent("sab:preference", { detail: v }));
    } catch (_) {}
    return v;
  }

  // Pure capability decision for a KNOWN preference: hosts with their own
  // preference sources (parent window props, legacy hashes, native shells)
  // parse those themselves, then call decide() instead of resolve().
  function decide(requested) {
    requested = sanitize(requested);
    var sab = detect();
    var effective = requested === "off" ? "single"
      : requested === "on" ? "multi"
      : sab.supported ? "multi" : "single";
    var reason = requested === "off" ? "user forced single-thread"
      : requested === "on"
        ? (sab.supported ? "user forced multi-thread" : "user forced multi-thread, but " + sab.reason)
        : sab.supported ? "auto: shared memory available"
        : "auto: " + sab.reason + " — falling back to single-thread";
    var out = { requested: requested, effective: effective, sab: sab.supported,
      detect: sab, reason: reason };
    try {
      root.dispatchEvent(new CustomEvent("sab:mode", { detail: out }));
    } catch (_) {}
    return out;
  }

  // Resolve pick + capability into what the app should boot.
  function resolve(prefer) {
    return decide(getPreference(prefer));
  }

  // Only register the COI service worker when threads could be on: with
  // threads=off there is nothing to isolate, and skipping avoids the SW's
  // one-time reload loop on platforms that can never isolate (iOS file://).
  // Returns "already" | "registered" | "skipped". The SW script itself
  // handles registration + reload; pass the same `coi` flags it supports.
  function ensureIsolation(swUrl, opts) {
    opts = opts || {};
    if (opts.want === false) return "skipped";
    try {
      if (root.crossOriginIsolated) return "already";
      if (!root.isSecureContext) return "skipped";
      if (!(root.navigator && root.navigator.serviceWorker)) return "skipped";
      root.coi = opts.coi;
      var s = document.createElement("script");
      s.src = swUrl;
      document.currentScript
        ? document.currentScript.parentNode.insertBefore(s, document.currentScript.nextSibling)
        : document.head.appendChild(s);
      return "registered";
    } catch (_) {
      return "skipped";
    }
  }

  // HEAD-probe a file, rejecting SPA-fallback / 404 HTML pages (vite dev and
  // static hosts answer 200 + text/html for missing files — instantiating
  // one as wasm/js is a confusing CompileError). Resolves true only for a
  // real non-HTML asset.
  function probeFile(url) {
    return fetch(url, { method: "HEAD" }).then(function (r) {
      var ct = (r.headers.get("content-type") || "").toLowerCase();
      return !!(r.ok && ct.indexOf("html") === -1);
    }).catch(function () { return false; });
  }

  // Pick which engine directory to boot. `isSt` tells whether we are ALREADY
  // on the single-thread build (so "off" needs no redirect). Resolves:
  //   { url }      — navigate here (ST build exists next door)
  //   { missing }  — ST build absent; show a build panel, do NOT boot MT
  //   { boot }     — boot the current engine as-is (multi, or already ST)
  // Pass stEntry as a bootability SENTINEL (e.g. "../linux-st/.bootable",
  // created only after a verified shell boot) rather than out.js itself: an
  // engine that exists but cannot boot must not receive the redirect, or the
  // user lands on an async crash past every try/catch.
  function pickVariant(o) {
    o = o || {};
    var mode = o.mode || resolve(o.prefer);
    var isSt = !!o.isSt;
    if (mode.effective === "multi") return Promise.resolve({ boot: true, mode: mode });
    if (isSt) return Promise.resolve({ boot: true, mode: mode });
    var stEntry = o.stEntry || "../linux-st/out.js";
    var stPage = o.stPage || "../linux-st/index.html";
    return probeFile(stEntry).then(function (ok) {
      if (ok) return { url: stPage + location.search + location.hash, mode: mode };
      return { missing: true, mode: mode };
    });
  }

  // One-line <select> wiring: keeps options auto/on/off in sync with the
  // stored preference and persists changes. Returns the select.
  function bindSelect(sel, onChange) {
    if (typeof sel === "string") sel = document.querySelector(sel);
    if (!sel) return null;
    try { sel.value = getPreference(sel.value === "auto" ? null : sel.value); } catch (_) {}
    sel.addEventListener("change", function () {
      var v = setPreference(sel.value);
      try { onChange && onChange(v); } catch (_) {}
    });
    try {
      root.addEventListener("sab:preference", function (e) {
        if (sel.value !== e.detail) sel.value = e.detail;
      });
    } catch (_) {}
    return sel;
  }

  // Short human strings for badges: { text, single:boolean }.
  function describe(mode, o) {
    o = o || {};
    if (mode.effective === "multi") {
      return { text: o.multiText || "threads: MTTCG-4 (SharedArrayBuffer)", single: false };
    }
    return { text: o.isSt ? (o.stText || "threads: single (dedicated ST engine)")
      : (o.missingText || "threads: single — needs linux-st build"), single: true };
  }

  root.SabToggle = {
    KEY: KEY,
    detect: detect,
    getPreference: getPreference,
    setPreference: setPreference,
    decide: decide,
    resolve: resolve,
    ensureIsolation: ensureIsolation,
    probeFile: probeFile,
    pickVariant: pickVariant,
    bindSelect: bindSelect,
    describe: describe,
  };
})(typeof window !== "undefined" ? window : this);
