import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const LINUX_DIR = path.resolve(__dirname, "../public/linux");

const MIME = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".wasm": "application/wasm", ".data": "application/octet-stream", ".json": "application/json" };
const server = http.createServer((req, res) => {
  let p = decodeURIComponent(req.url.split("?")[0]);
  if (p === "/") p = "/index.html";
  fs.readFile(path.join(LINUX_DIR, p), (err, buf) => {
    if (err) { res.writeHead(404); res.end("nf"); return; }
    res.writeHead(200, { "Content-Type": MIME[path.extname(p)] || "application/octet-stream" });
    res.end(buf);
  });
});
await new Promise((r) => server.listen(8099, r));

const browser = await puppeteer.launch({ headless: true, executablePath: "/usr/bin/google-chrome-stable", args: ["--no-sandbox", "--disable-setuid-sandbox", "--disable-gpu"] });
const page = await browser.newPage();
const errors = [];
page.on("pageerror", (e) => errors.push("PAGEERROR: " + e.message));
page.on("console", (m) => { if (m.type() === "error") errors.push("CONSOLE: " + m.text()); });

await page.goto("http://127.0.0.1:8099/", { waitUntil: "load" });

const textOf = () => page.evaluate(() => { const r = document.querySelector(".xterm-rows"); return r ? r.innerText : ""; });

let sawShell = false;
for (let i = 0; i < 180; i++) {
  await new Promise((r) => setTimeout(r, 1000));
  if (/~ #/.test(await textOf())) { sawShell = true; break; }
}
if (!sawShell) { console.log("NO SHELL"); await browser.close(); server.close(); process.exit(1); }

// focus the terminal and type a command
await page.focus(".xterm-helper-textarea").catch(() => {});
await page.keyboard.type("echo pi3-emu-ok");
await page.keyboard.press("Enter");

let typed = false;
for (let i = 0; i < 20; i++) {
  await new Promise((r) => setTimeout(r, 1000));
  if (/pi3-emu-ok/.test(await textOf())) { typed = true; break; }
}

await browser.close();
server.close();
console.log("sawShell:", sawShell, "typedEcho:", typed);
if (errors.length) console.log("ERRORS:\n" + errors.join("\n"));
process.exit(sawShell && typed && errors.length === 0 ? 0 : 1);
