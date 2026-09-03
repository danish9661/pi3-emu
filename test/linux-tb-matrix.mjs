import puppeteer from "puppeteer-core";
import { spawn } from "node:child_process";
import http from "node:http";
import { fileURLToPath } from "node:url";
import path from "node:path";

// tb-size tuning matrix on the MT engine (direct linux/ URLs, threads=on).
// Usage: node test/linux-tb-matrix.mjs [128,256,500,1024]
// Prints one JSON line per tb: {tb, shellAt, phases, errors}.
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PORT = 5176;
const URL = `http://localhost:${PORT}/`;
const TBS = (process.argv[2] || "128,256,500").split(",").map((s) => s.trim()).filter(Boolean);
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
  protocolTimeout: 240000,
  args: ["--no-sandbox", "--disable-setuid-sandbox", "--disable-gpu", "--disable-dev-shm-usage"] });
try {
  for (const tb of TBS) {
    const rep = { tb, shellAt: -1, phases: {}, errors: [] };
    const page = await browser.newPage();
    page.on("pageerror", (e) => rep.errors.push("PAGEERROR: " + String((e && e.message) || e).slice(0, 100)));
    page.on("console", (m) => { if (m.type() === "error") rep.errors.push("CONSOLE: " + m.text().slice(0, 100)); });
    try {
      await page.goto(`${URL}linux/index.html#cfg=minimal&threads=on&tb=${tb}`, { waitUntil: "load" });
      const t0 = Date.now();
      for (let i = 0; i < 210; i++) {
        await wait(2000);
        const st = await page.evaluate(() => {
          const rows = document.querySelector(".xterm-rows");
          return { txt: rows ? rows.innerText.slice(0, 60) : "",
            phases: document.getElementById("boot-phases")?.innerText || "" };
        }).catch(() => null);
        if (!st) continue;
        for (const m of st.phases.matchAll(/(download|engine|kernel|userspace|shell)\s+T\+(\d+)s/g)) {
          if (!(m[1] in rep.phases)) rep.phases[m[1]] = Number(m[2]);
        }
        if (/~ ?#/.test(st.txt)) { rep.shellAt = Math.round((Date.now() - t0) / 1000); break; }
      }
    } catch (e) { rep.errors.push("HARNESS: " + String(e).slice(0, 100)); }
    console.log(JSON.stringify(rep));
    await page.close().catch(() => {});
  }
} finally {
  await browser.close();
}
cleanup();
