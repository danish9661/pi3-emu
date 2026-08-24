import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import puppeteer from "puppeteer-core";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const LINUX_DIR = path.resolve(__dirname, "../public/linux");

const MIME = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".wasm": "application/wasm",
  ".data": "application/octet-stream",
  ".json": "application/json",
};
const server = http.createServer((req, res) => {
  let p = decodeURIComponent(req.url.split("?")[0]);
  if (p === "/") p = "/index.html";
  const fp = path.join(LINUX_DIR, p);
  fs.readFile(fp, (err, buf) => {
    if (err) { res.writeHead(404); res.end("nf"); return; }
    res.writeHead(200, { "Content-Type": MIME[path.extname(fp)] || "application/octet-stream" });
    res.end(buf);
  });
});
await new Promise((r) => server.listen(8099, r));
const url = "http://127.0.0.1:8099/";

const browser = await puppeteer.launch({
  headless: true,
  executablePath: "/usr/bin/google-chrome-stable",
  args: ["--no-sandbox", "--disable-setuid-sandbox", "--disable-gpu"],
});
const page = await browser.newPage();
const errors = [];
page.on("pageerror", (e) => errors.push("PAGEERROR: " + e.message));
page.on("console", (m) => { if (m.type() === "error") errors.push("CONSOLE: " + m.text()); });
page.on("response", (r) => { if (r.status() === 404) errors.push("404: " + r.url()); });

await page.goto(url, { waitUntil: "load" });

let sawPrompt = false, sawShell = false, sentFallback = false;
for (let i = 0; i < 420; i++) {
  await new Promise((r) => setTimeout(r, 1000));
  const txt = await page.evaluate(() => {
    const r = document.querySelector(".xterm-rows");
    return r ? r.innerText : "";
  });
  if (/Please press Enter to activate this console\./.test(txt)) sawPrompt = true;
  if (/~ ?#/.test(txt)) { sawShell = true; break; }
}

await browser.close();
server.close();

console.log("sawPrompt:", sawPrompt, "sawShell:", sawShell, "fallbackSent:", sentFallback);
if (errors.length) console.log("ERRORS:\n" + errors.join("\n"));
else console.log("no page errors");
process.exit(sawShell && errors.length === 0 ? 0 : 1);
