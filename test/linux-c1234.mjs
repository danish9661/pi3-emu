import puppeteer from "puppeteer-core";
import { spawn } from "node:child_process";
import http from "node:http";
import { fileURLToPath } from "node:url";
import path from "node:path";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const PORT = 5173;
const URL = `http://localhost:${PORT}/`;

const wait = (ms) => new Promise((r) => setTimeout(r, ms));
async function serverReady() {
  for (let i = 0; i < 60; i++) {
    await wait(1000);
    const ok = await new Promise((res) => http.get(URL, (r) => { r.destroy(); res(r.statusCode === 200); }).on("error", () => res(false)));
    if (ok) return true;
  }
  return false;
}

const vite = spawn("npm", ["run", "dev"], { cwd: ROOT, stdio: "ignore" });
const cleanup = () => { try { vite.kill("SIGTERM"); } catch {} };
process.on("exit", cleanup);

if (!(await serverReady())) { console.log("vite did not start"); cleanup(); process.exit(1); }

const browser = await puppeteer.launch({ headless: true, executablePath: "/usr/bin/google-chrome-stable", args: ["--no-sandbox", "--disable-setuid-sandbox", "--disable-gpu"] });
const page = await browser.newPage();
const errors = [];
page.on("pageerror", (e) => errors.push("PAGEERROR: " + e.message));
page.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errors.push("CONSOLE: " + m.text()); });
page.on("response", (r) => { if (r.status() === 404 && !/favicon\.ico/.test(r.url())) errors.push("404: " + r.url()); });

const frameText = () => page.evaluate(() => {
  const f = document.getElementById("linuxframe");
  if (!f) return "";
  try { const r = f.contentWindow.document.querySelector(".xterm-rows"); return r ? r.innerText : ""; } catch { return ""; }
}).catch(() => "");

// boot via the real UI (select linux + Run)
await page.goto(URL, { waitUntil: "load" });
await page.select("#prog", "linux");
await page.click("#run");

let sawShell = false;
for (let i = 0; i < 600; i++) {
  await wait(1000);
  if (/~ ?#/.test(await frameText())) { sawShell = true; break; }
}
console.log("sawShell:", sawShell);

// C1: run the `hw` tour from the browser command box
let hwOk = false;
if (sawShell) {
  await page.evaluate(() => {
    const f = document.getElementById("linuxframe").contentWindow;
    const cmd = f.document.getElementById("cmd");
    cmd.value = "hw";
    f.document.getElementById("cmdBtn").click();
  });
  for (let i = 0; i < 20; i++) {
    await wait(1000);
    if (/pi3-emu hardware tour/.test(await frameText())) { hwOk = true; break; }
  }
}
console.log("hwTour:", hwOk);

// C4: click the GPIO21 on button, expect the echoed result
let gpioOk = false;
if (sawShell) {
  await page.evaluate(() => document.getElementById("linuxframe").contentWindow.document.getElementById("gpioOn").click());
  for (let i = 0; i < 20; i++) {
    await wait(1000);
    if (/GPIO21 -> 1/.test(await frameText())) { gpioOk = true; break; }
  }
}
console.log("gpioButton:", gpioOk);

await browser.close();
cleanup();
console.log("ERRORS:", errors.length ? "\n" + errors.join("\n") : "none");
const pass = sawShell && hwOk && gpioOk && errors.length === 0;
console.log("RESULT:", pass ? "PASS" : "FAIL");
process.exit(pass ? 0 : 1);
