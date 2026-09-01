// M30 Browser E2E test: Playwright + headless Chrome
// Tests the browser UI: dropdown, Run button, terminal output, periphs device
// verification, and regression on existing programs.
//
// Usage: node test/m30-e2e.mjs  (vite dev server must be running on :5173)

import { chromium } from 'playwright';
import { dirname } from 'path';
import { fileURLToPath } from 'url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const URL = 'http://localhost:5173/';

const browser = await chromium.launch({
  headless: true,
  executablePath: '/usr/bin/google-chrome-stable',
  args: ['--no-sandbox', '--disable-gpu'],
});

const results = {};

async function testProgram(page, progName, opts = {}) {
  const { wantStrings = [], timeout = 30000, expectDone = true } = opts;

  await page.selectOption('#prog', progName);
  await page.click('#run');

  // Wait for runBtn to cycle: disabled (during run) -> enabled (after run).
  // Status text may still contain old "booted" from the prior run, so we
  // rely on the button state instead.
  await page.waitForFunction(
    () => {
      const btn = document.getElementById('run');
      const s = document.getElementById('status')?.textContent || '';
      if (s.startsWith('ERROR:')) return true;
      return !btn.disabled;
    },
    { timeout }
  );

  await page.waitForTimeout(300);
  const term = await page.evaluate(() => document.getElementById('term')?.textContent || '');
  const status = await page.evaluate(() => document.getElementById('status')?.textContent || '');
  const found = wantStrings.every((w) => term.includes(w));
  results[progName] = { found, term: term.slice(-500), status };
  return { found, term };
}

const page = await browser.newPage();
const jsErrors = [];
page.on('pageerror', (e) => jsErrors.push(e.message));

console.log('M30 E2E: loading page...');
await page.goto(URL, { waitUntil: 'domcontentloaded' });

await page.waitForFunction(
  () => {
    const btn = document.getElementById('run');
    const s = document.getElementById('status')?.textContent || '';
    return (!btn.disabled) || s.includes('ERROR');
  },
  { timeout: 20000 }
);
console.log('M30 E2E: page loaded, initial run complete');

const hasPeriphs = await page.evaluate(() => {
  const sel = document.getElementById('prog');
  return Array.from(sel.options).some((o) => o.value === 'periphs');
});
console.log(`M30 E2E: dropdown has "periphs" option: ${hasPeriphs}`);

console.log('\n=== Testing periphs ===');
await testProgram(page, 'periphs', { wantStrings: ['periphs: ALL PASS'], timeout: 60000 });
console.log(results['periphs'].found ? '  PASS' : '  FAIL');
if (!results['periphs'].found) {
  console.log('  terminal:', results['periphs'].term.replace(/\r/g, ''));
  console.log('  status:', results['periphs'].status);
}

console.log('\n=== Testing shell (regression) ===');
await testProgram(page, 'shell', { wantStrings: ['Hi'], timeout: 15000, expectDone: false });
console.log(results['shell'].found ? '  PASS' : '  FAIL');

console.log('\n=== Testing spi (regression) ===');
await testProgram(page, 'spi', { wantStrings: ['spi: all checks passed'], timeout: 15000 });
console.log(results['spi'].found ? '  PASS' : '  FAIL');

console.log('\n=== Testing uart1 (regression) ===');
await testProgram(page, 'uart1', { wantStrings: ['uart1:'], timeout: 15000, expectDone: false });
console.log(results['uart1'].found ? '  PASS' : '  FAIL');

console.log('\n=== Testing i2c (regression) ===');
await testProgram(page, 'i2c', { wantStrings: ['i2c: all checks passed'], timeout: 15000 });
console.log(results['i2c'].found ? '  PASS' : '  FAIL');

console.log('\n=== Testing pwm (regression) ===');
await testProgram(page, 'pwm', { wantStrings: ['pwm:'], timeout: 15000, expectDone: false });
console.log(results['pwm'].found ? '  PASS' : '  FAIL');

console.log('\n=== Testing linux mode ===');
await page.selectOption('#prog', 'linux');
await page.click('#run');
await page.waitForTimeout(5000);
const linuxIframe = await page.evaluate(() => {
  const f = document.getElementById('linuxframe');
  return f ? { hidden: f.hidden, src: f.src } : null;
});
const linuxOk = linuxIframe && !linuxIframe.hidden;
console.log(linuxOk ? '  PASS: linux iframe visible' : '  FAIL: linux iframe not found or hidden');
if (linuxIframe) console.log('  iframe src:', linuxIframe.src);

console.log('\n=== JS Errors ===');
console.log(jsErrors.length === 0 ? '  None' : jsErrors.map((e) => '  ERROR: ' + e).join('\n'));

console.log('\n=== Summary ===');
let pass = 0, fail = 0;
for (const [k, v] of Object.entries(results)) {
  console.log(v.found ? `  ✓ ${k}` : `  ✗ ${k}`);
  v.found ? pass++ : fail++;
}
if (hasPeriphs) pass++; else fail++;
console.log(`\n${pass}/${pass + fail} passed, ${jsErrors.length} JS errors`);

await browser.close();
process.exit(fail === 0 && jsErrors.length === 0 ? 0 : 1);
