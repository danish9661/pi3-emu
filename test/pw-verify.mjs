// Playwright end-to-end verification for the pi-cpu demo page.
// Boots every bare-metal program, exercises keys/button/REPL/canvas,
// asserts zero page errors. Requires `npx vite preview` on :5173
// (or set BASE_URL). Usage: node test/pw-verify.mjs
import { chromium } from 'playwright';

const BASE = process.env.BASE_URL || 'http://localhost:5173/';
const TIMEOUT = 90000;
const results = [];
const ok = (name, cond, extra = '') => {
  results.push([cond ? 'ok' : 'FAIL', name, extra]);
  console.log(cond ? 'ok' : 'FAIL', name, extra);
};

const browser = await chromium.launch();
const page = await browser.newPage();
const errors = [];
page.on('pageerror', (e) => errors.push('PAGEERROR: ' + String(e).split('\n')[0].slice(0, 120)));
page.on('console', (m) => { if (m.type() === 'error') errors.push('CONSOLE: ' + m.text().slice(0, 120)); });
await page.goto(BASE, { waitUntil: 'networkidle', timeout: 60000 });

async function boot(prog) {
  await page.selectOption('#prog', prog);
  await page.click('#run');
  await page.waitForFunction(
    () => /booted|warn:|ERROR|guest fault/.test(document.getElementById('status').textContent),
    { timeout: TIMEOUT }
  );
  await page.waitForTimeout(2500);
  return {
    status: await page.$eval('#status', (e) => e.textContent),
    term: await page.$eval('#term', (e) => e.textContent),
  };
}

// Sync guests: boot text goldens.
const goldens = {
  shell: 'Hi\n>',
  sum: 'sum demo',
  fib: 'fib',
  smp: 'all cores joined: counter = 4',
  clock: 'C1 match',
  gpio: 'chase done',
  fb: 'pattern drawn',
  irq: '[irq #1',
  lirq: 'A and B delivered',
  mmu: 'all checks passed',
  dma: 'all checks passed',
  pwm: 'all notes played',
  i2c: 'all checks passed',
  spi: 'all checks passed',
  uart1: '[u1]',
  sd: 'payload matches',
  uart0: 'RXINTR armed',
  upython: '>>>',
  periphs: 'ALL PASS',
  debug: 'ALL PASS',
  bench: 'benchmark',
};
for (const [prog, want] of Object.entries(goldens)) {
  const { status, term } = await boot(prog);
  ok(`boot ${prog}`, status.startsWith('booted') && term.includes(want), status.slice(0, 60));
}

// Interactive: shell typing.
await boot('shell');
await page.click('#term');
await page.keyboard.type('HI');
await page.keyboard.press('Enter');
await page.waitForTimeout(2000);
let term = await page.$eval('#term', (e) => e.textContent);
ok('shell HI->HELLO', term.includes('HELLO'));

// Interactive: upython int + float.
await boot('upython');
await page.click('#term');
await page.keyboard.type('1+1');
await page.keyboard.press('Enter');
await page.waitForTimeout(3000);
term = await page.$eval('#term', (e) => e.textContent);
ok('upy 1+1->2', term.includes('\r\n2\r\n'));
await page.keyboard.type('1.5+2.25');
await page.keyboard.press('Enter');
await page.waitForTimeout(3000);
term = await page.$eval('#term', (e) => e.textContent);
ok('upy float->3.75', term.includes('3.75'));

// Interactive: gpio button IRQ.
await boot('gpio');
await page.hover('#gpio-btn');
await page.mouse.down();
await page.waitForTimeout(2500);
await page.mouse.up();
await page.waitForTimeout(2500);
term = await page.$eval('#term', (e) => e.textContent);
ok('gpio button IRQ', /IRQ on BTN 29/.test(term));

// fb canvas actually lit.
await boot('fb');
await page.waitForTimeout(3000);
const px = await page.evaluate(() => {
  const c = document.getElementById('fbscreen');
  const d = c.getContext('2d').getImageData(0, 0, c.width, c.height).data;
  let n = 0;
  for (let i = 0; i < d.length; i += 4) if (d[i] + d[i + 1] + d[i + 2] > 0) n++;
  return [c.width, c.height, n];
});
ok('fb canvas lit', px[0] === 160 && px[1] === 120 && px[2] > 1000, `${px[0]}x${px[1]} lit=${px[2]}`);

ok('zero page errors', errors.length === 0, errors.slice(0, 5).join(' | '));
await browser.close();

const fails = results.filter(([s]) => s === 'FAIL');
console.log(`${results.length - fails.length} ok, ${fails.length} FAIL`);
process.exit(fails.length ? 1 : 0);
