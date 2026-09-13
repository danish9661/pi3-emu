// Playwright Linux (kernel demo) E2E: full app flow select-linux -> Run ->
// iframe boots qemu-wasm raspi3ap to a busybox shell, then an interactive
// echo round-trips. Slow by nature (TCG boot, minutes); polls patiently.
// Requires `npx vite preview` on :5173 (or BASE_URL).
// Usage: node test/pw-linux.mjs
import { chromium } from 'playwright';

const BASE = process.env.BASE_URL || 'http://localhost:5173/';
const BOOT_BUDGET_MS = Number(process.env.PW_LINUX_BOOT_MS || 600000);
const errors = [];
const log = (m) => console.log(m);

const browser = await chromium.launch();
const page = await browser.newPage();
page.on('pageerror', (e) => errors.push('PAGEERROR: ' + String(e).split('\n')[0].slice(0, 120)));
page.on('console', (m) => { if (m.type() === 'error') errors.push('CONSOLE: ' + m.text().slice(0, 120)); });

await page.goto(BASE, { waitUntil: 'networkidle', timeout: 60000 });
await page.selectOption('#prog', 'linux');
await page.click('#run');
await page.waitForSelector('#linuxframe', { timeout: 30000 });
log('ok iframe created');

const frame = page.frameLocator('#linuxframe');
await frame.locator('.xterm-rows').waitFor({ timeout: 120000 });
log('ok xterm up');

const textOf = () => frame.locator('.xterm-rows').innerText().catch(() => '');
let sawShell = false;
const t0 = Date.now();
while (Date.now() - t0 < BOOT_BUDGET_MS) {
  const t = await textOf();
  if (/~\s?#/.test(t)) { sawShell = true; break; }
  await page.waitForTimeout(5000);
}
log(sawShell ? 'ok shell prompt' : 'FAIL no shell in budget');
if (!sawShell) {
  log('tail: ' + JSON.stringify((await textOf()).slice(-300)));
  await browser.close();
  process.exit(1);
}

await frame.locator('.xterm-helper-textarea').focus().catch(() => {});
await page.keyboard.type('echo pi3-emu-ok');
await page.keyboard.press('Enter');
let typed = false;
for (let i = 0; i < 60; i++) {
  await page.waitForTimeout(1000);
  if (/pi3-emu-ok/.test(await textOf())) { typed = true; break; }
}
log(typed ? 'ok echo round-trip' : 'FAIL no echo');
log(errors.length ? 'ERRORS:\n' + errors.join('\n') : 'ok zero page errors');
await browser.close();
process.exit(sawShell && typed && errors.length === 0 ? 0 : 1);
