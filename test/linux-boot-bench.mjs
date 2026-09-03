import puppeteer from "puppeteer-core";
import { spawn } from "node:child_process";
import http from "node:http";
import { fileURLToPath } from "node:url";
import path from "node:path";

// Phase-level Linux boot benchmark. Boots the real harness (default:
// threads=auto -> MTTCG here) and prints JSON with per-phase T+ times and
// time-to-shell. A/B usage:
//   node test/linux-boot-bench.mjs            # auto (fast engine if SAB)
//   node test/linux-boot-bench.mjs threads=off  # single-thread engine
// Compare the "shellAt" + phases across runs (same machine, warm cache for
// run 2+; note cold-vs-warm wasm compile in the report).
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PORT = 5175;
const URL = `http://localhost:${PORT}/`;
const THREADS = (process.argv.find((a) => a.startsWith("threads=")) || "threads=auto").split("=")[1];

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
if (!(await serverReady())) { console.log(JSON.stringify({ error: "vite did not start" })); cleanup(); process.exit(1); }

const browser = await puppeteer.launch({ headless: true, executablePath: "/usr/bin/google-chrome-stable",
  args: ["--no-sandbox", "--disable-setuid-sandbox", "--disable-gpu", "--disable-dev-shm-usage"] });
const report = { threads: THREADS, phases: {}, shellAt: -1, errors: [], sab: null, effective: null };
try {
  const page = await browser.newPage();
  page.on("pageerror", (e) => report.errors.push("PAGEERROR: " + e.message.slice(0, 120)));
  page.on("console", (m) => { if (m.type() === "error") report.errors.push("CONSOLE: " + m.text().slice(0, 120)); });
  await page.goto(URL, { waitUntil: "load" });
  await wait(2000);
  await page.select("#prog", "linux");
  await wait(300);
  await page.select("#linuxThreads", THREADS);
  await page.click("#run");
  const t0 = Date.now();
  for (let i = 0; i < 240; i++) {
    await wait(2000);
    const st = await page.evaluate(() => {
      const f = document.getElementById("linuxframe");
      if (!f) return null;
      try {
        const w = f.contentWindow, d = w.document;
        const rows = d.querySelector(".xterm-rows");
        return {
          txt: rows ? rows.innerText.slice(0, 60) : "",
          phases: d.getElementById("boot-phases")?.innerText || "",
          eff: w.__pi3ThreadsEffective, sab: w.__pi3Sab,
          fbHidden: d.getElementById("boot-fallback")?.hidden,
        };
      } catch (e) { return null; }
    }).catch(() => null);
    if (!st) continue;
    if (report.effective === null && st.eff) { report.effective = st.eff; report.sab = st.sab; }
    for (const m of st.phases.matchAll(/(download|engine|kernel|userspace|shell)\s+T\+(\d+)s/g)) {
      if (!(m[1] in report.phases)) report.phases[m[1]] = Number(m[2]);
    }
    if (/~ ?#/.test(st.txt)) {
      report.shellAt = Math.round((Date.now() - t0) / 1000);
      report.fallbackHidden = st.fbHidden;
      break;
    }
    if (st.fbHidden === false) { report.blocked = "fallback panel (no ST build?)"; break; }
  }
} finally {
  await browser.close();
}
cleanup();
console.log(JSON.stringify(report, null, 1));
process.exit(report.shellAt > 0 || report.blocked ? 0 : 1);
