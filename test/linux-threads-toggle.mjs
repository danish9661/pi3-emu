import puppeteer from "puppeteer-core";
import { spawn } from "node:child_process";
import http from "node:http";
import { fileURLToPath } from "node:url";
import path from "node:path";

// Threads toggle + phased boot progress (fast: no full kernel boot).
// - #linuxThreads auto/on/off visible only for the linux program
// - auto + SAB => qemu thread=multi, MTTCG badge, phases + T+ clock
// - threads=off => engine gated (pthread build cannot run single) with a
//   build panel pointing at scripts/build-linux.sh --threads=st, zero errors
// - legacy #minimal hash URL still boots the multi engine
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PORT = 5174;
const URL = `http://localhost:${PORT}/`;

const wait = (ms) => new Promise((r) => setTimeout(r, ms));
async function serverReady() {
  for (let i = 0; i < 60; i++) {
    await wait(1000);
    const ok = await new Promise((res) =>
      http.get(URL, (r) => { r.destroy(); res(r.statusCode === 200); }).on("error", () => res(false)));
    if (ok) return true;
  }
  return false;
}

const vite = spawn("npm", ["run", "dev", "--", "--port", String(PORT), "--host", "127.0.0.1"],
  { cwd: ROOT, stdio: "ignore" });
const cleanup = () => { try { vite.kill("SIGTERM"); } catch {} };
process.on("exit", cleanup);

const fails = [];
const check = (name, cond, extra = "") => {
  console.log((cond ? "PASS " : "FAIL ") + name + (extra ? " | " + extra : ""));
  if (!cond) fails.push(name);
};

if (!(await serverReady())) { console.log("vite did not start"); cleanup(); process.exit(1); }

const browser = await puppeteer.launch({ headless: true, executablePath: "/usr/bin/google-chrome-stable",
  args: ["--no-sandbox", "--disable-setuid-sandbox", "--disable-gpu", "--disable-dev-shm-usage"] });
try {
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push("PAGEERROR: " + e.message.slice(0, 160)));
  page.on("console", (m) => { if (m.type() === "error") errors.push("CONSOLE: " + m.text().slice(0, 160)); });
  await page.goto(URL, { waitUntil: "load" });
  await wait(2000);

  // Reusable library present on both pages (the any-project drop-in).
  const lib = await page.evaluate(() => ({
    root: !!window.SabToggle,
    fns: window.SabToggle ? ["detect", "getPreference", "setPreference", "decide", "resolve", "pickVariant", "bindSelect", "describe"].filter((f) => typeof window.SabToggle[f] === "function").length : 0,
    supported: window.SabToggle ? window.SabToggle.detect().supported : null,
  }));
  check("SabToggle library on root page", lib.root && lib.fns === 8, JSON.stringify(lib));
  check("SabToggle.detect() supported here", lib.supported === true);

  const sel0 = await page.evaluate(() => ({
    exists: !!document.getElementById("linuxThreads"),
    display: getComputedStyle(document.getElementById("linuxThreads")).display,
    opts: [...document.getElementById("linuxThreads").options].map((o) => o.value).join(","),
  }));
  check("threads selector hidden by default", sel0.exists && sel0.display === "none");
  check("threads options auto/on/off", sel0.opts === "auto,on,off", sel0.opts);
  await page.select("#prog", "linux");
  await wait(400);
  const sel1 = await page.evaluate(() => ({
    cfg: getComputedStyle(document.getElementById("linuxConfig")).display,
    thr: getComputedStyle(document.getElementById("linuxThreads")).display,
  }));
  check("both selects visible for linux", sel1.cfg !== "none" && sel1.thr !== "none");

  await page.click("#run");
  await wait(3500);
  const f1 = await page.evaluate(() => {
    const f = document.getElementById("linuxframe");
    const w = f.contentWindow;
    return {
      src: f.src,
      args: (w.Module && w.Module.pi3Args) || null,
      req: w.__pi3ThreadsRequested, eff: w.__pi3ThreadsEffective, sab: w.__pi3Sab,
      badge: w.document.getElementById("thread-badge")?.textContent || "",
      phases: !!w.document.getElementById("boot-phases"),
      clock: w.document.getElementById("boot-clock")?.textContent || "",
    };
  });
  check("iframe url carries cfg+threads", /#cfg=minimal&threads=auto/.test(f1.src), f1.src);
  check("auto+SAB => multi", f1.eff === "multi" && f1.args.join(" ").includes("thread=multi"));
  check("badge shows MTTCG", /MTTCG-4/.test(f1.badge), f1.badge);
  check("phases+T+ clock present", f1.phases && /^T\+\d+s$/.test(f1.clock), f1.clock);

  await page.select("#linuxThreads", "off");
  await page.click("#run");
  await wait(3500);
  const f2 = await page.evaluate(() => {
    const w = document.getElementById("linuxframe").contentWindow;
    return {
      src: document.getElementById("linuxframe").src,
      req: w.__pi3ThreadsRequested, eff: w.__pi3ThreadsEffective, needSt: w.__pi3NeedSt,
      badge: w.document.getElementById("thread-badge")?.textContent || "",
      fbVisible: !w.document.getElementById("boot-fallback")?.hidden,
      fbHint: w.document.getElementById("fallback-hint")?.textContent || "",
    };
  });
  check("threads=off in url", /threads=off/.test(f2.src), f2.src);
  check("off => needSt routing", f2.eff === "single" && f2.needSt === true);
  check("badge asks for linux-st", /linux-st/.test(f2.badge), f2.badge);
  check("fallback panel visible", f2.fbVisible && /--threads=st/.test(f2.fbHint));

  // Library inside the iframe too, and the pick persists across reloads.
  const libF = await page.evaluate(() => {
    const w = document.getElementById("linuxframe").contentWindow;
    return { present: !!w.SabToggle, mode: w.SabToggle ? w.SabToggle.resolve().requested : null };
  });
  check("SabToggle library in iframe", libF.present && libF.mode === "off", JSON.stringify(libF));
  await page.goto(URL, { waitUntil: "load" });
  await wait(1500);
  const persisted = await page.evaluate(() => document.getElementById("linuxThreads").value);
  check("threads pick persists via localStorage", persisted === "off", persisted);
  await page.evaluate(() => { window.SabToggle.setPreference("auto"); });

  await page.goto(URL + "linux/index.html#minimal", { waitUntil: "load" });
  await wait(1500);
  const leg = await page.evaluate(() => ({
    args: (window.Module && window.Module.pi3Args) || null,
    eff: window.__pi3ThreadsEffective,
  }));
  check("legacy #minimal intact", leg.args && leg.args.join(" ").includes("thread=multi"));

  check("zero page errors", errors.length === 0, errors.slice(0, 4).join(" ;; "));
} finally {
  await browser.close();
}
cleanup();
console.log(fails.length ? "RESULT FAIL: " + fails.join(", ") : "RESULT ALL PASS");
process.exit(fails.length ? 1 : 0);
