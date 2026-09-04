import puppeteer from "puppeteer-core";
import { spawn } from "node:child_process";
import http from "node:http";
import { fileURLToPath } from "node:url";
import path from "node:path";

// PWM audio path: worklet engine + exact 84672-sample stream, no page errors.
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PORT = 5177;
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
  args: ["--no-sandbox", "--disable-setuid-sandbox", "--disable-gpu", "--disable-dev-shm-usage",
    "--autoplay-policy=no-user-gesture-required", "--mute-audio"] });
try {
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push("PAGEERROR: " + e.message.slice(0, 160)));
  page.on("console", (m) => { if (m.type() === "error") errors.push("CONSOLE: " + m.text().slice(0, 160)); });
  await page.goto(URL, { waitUntil: "load" });
  await wait(2000);
  await page.select("#prog", "pwm");
  await wait(300);
  await page.click("#run");
  let parked = false, engine = "", fed = -1;
  for (let i = 0; i < 240; i++) {
    await wait(2000);
    const st = await page.evaluate(() => ({
      status: document.getElementById("status")?.textContent || "",
      term: document.getElementById("term")?.textContent || "",
      engine: window.__pwmEngine || "",
      fed: window.__pwmFed ?? -1,
    })).catch(() => null);
    if (!st) continue;
    engine = st.engine; fed = st.fed;
    if (/parked/.test(st.term)) { parked = true; break; }
  }
  check("pwm guest parks", parked);
  check("audio engine is worklet", engine === "worklet", engine);
  check("exact 84672 samples posted", fed === 84672, String(fed));
  check("zero page errors", errors.length === 0, errors.slice(0, 4).join(" ;; "));
} finally {
  await browser.close();
}
cleanup();
console.log(fails.length ? "RESULT FAIL: " + fails.join(", ") : "RESULT ALL PASS");
process.exit(fails.length ? 1 : 0);
