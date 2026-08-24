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
    await new Promise((r) => setTimeout(r, 1000));
    await new Promise((res) => http.get(URL, (r) => { r.destroy(); res(true); }).on("error", () => res(false)));
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

// drive the real UI: select the linux program, click Run
await page.goto(URL, { waitUntil: "load" });
await page.select("#prog", "linux");
await page.click("#run");

const textOfFrame = async () => {
  return page.evaluate(() => {
    const f = document.getElementById("linuxframe");
    if (!f) return "";
    try { const r = f.contentWindow.document.querySelector(".xterm-rows"); return r ? r.innerText : ""; }
    catch { return ""; }
  }).catch(() => "");
};

let sawShell = false;
for (let i = 0; i < 240; i++) {
  await wait(1000);
  const t = await textOfFrame();
  if (/~ #/.test(t)) { sawShell = true; break; }
}

await browser.close();
cleanup();
console.log("sawShellViaUI:", sawShell);
if (errors.length) console.log("ERRORS:\n" + errors.join("\n"));
process.exit(sawShell && errors.length === 0 ? 0 : 1);
