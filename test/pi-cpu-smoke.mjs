// pi-cpu golden smoke: the post-unicorn regression battery. Runs each
// guest on target/release/examples/run (pi-cpu only — no oracle) and
// asserts golden console substrings + fault presence/absence + (fb) the
// framebuffer hash. SMP runs with PI3_SMP=1; firmware boots only (deep
// REPL coverage lives in test/upython-*.mjs).
// Usage: node test/pi-cpu-smoke.mjs [filter]
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');
const RUN = join(ROOT, 'target', 'release', 'examples', 'run');
const filter = process.argv[2] || '';

if (!existsSync(RUN)) {
  console.log('SKIP: no run binary (cargo build --release --examples first)');
  process.exit(0);
}

function run(prog, args = [], env = {}) {
  const out = execFileSync(RUN, [join(ROOT, 'public', 'programs', `${prog}.elf`), ...args.map(String)],
    { maxBuffer: 64 * 1024 * 1024, env: { ...process.env, ...env } }).toString();
  return Object.fromEntries(out.trim().split('\n').map((l) => {
    const j = l.indexOf('\t');
    return [l.slice(0, j), l.slice(j + 1)];
  }));
}
const unesc = (s) => JSON.parse(`"${s}"`);

// [name, prog, args, env, { console: [...substrings], fault: null | 'present' | 'string-prefix', extra(L)->check }]
const CASES = [
  ['shell', 'shell', ['200000'], {}, { console: ['Hi', '> '], fault: 'null' }],
  ['sum', 'sum', ['200000'], {}, { console: ['sum demo'], fault: 'null' }],
  ['fib', 'fib', ['200000'], {}, { console: ['fib'], fault: 'null' }],
  ['smp', 'smp', ['20000000', '512'], { PI3_SMP: '1' }, {
    console: ['core 0: sum 1..25 = 325', 'core 3: sum 76..100 = 2200', 'all cores joined: counter = 4'],
    smp: ['15', '4', '325', '950', '1575', '2200', 'null'],
  }],
  ['clock', 'clock', ['400000', '4096', '262144'], {}, { console: ['C1 match', 'done'], fault: 'null' }],
  ['gpio', 'gpio', ['700000', '4096', '262144', '350000', '400000', '450000'], {},
    { console: ['chase done', 'IRQ phase done'], fault: 'null' }],
  ['fb', 'fb', ['409600', '4096', '262144'], {}, {
    console: ['fb: 160x120 pitch 640', 'pattern drawn'], fault: 'null',
    fb: '160x120 p640 hashcab23cec7a1775bb c0=000000ff c1=000000ff c2=00ff0000 cc=00000000',
  }],
  ['irq', 'irq', ['500000', '4096', '262144', '0', '0', '0', '72', '350000'], {},
    { console: ['[irq #1'], fault: 'null' }],
  ['lirq', 'lirq', ['400000', '4096', '262144', '0', '0', '0', '0', '0', '1'], {},
    { console: ['lirq: A and B delivered'], fault: 'null' }],
  ['mmu', 'mmu', ['200000'], {}, { console: ['mmu'], fault: 'null' }],
  ['mva', 'mva', ['200000'], {}, { console: ['PASS'], fault: 'null' }],
  ['dma', 'dma', ['300000'], {}, { console: ['dma'], fault: 'null' }],
  ['pwm', 'pwm', ['1000000', '512'], {}, { console: ['pwm:'], fault: 'null' }],
  ['i2c', 'i2c', ['1000000'], {}, { console: ['i2c'], fault: 'null' }],
  ['spi', 'spi', ['1000000'], {}, { console: ['spi'], fault: 'null' }],
  ['uart1', 'uart1', ['200000'], {}, { console: ['[u1]'], fault: 'null' }],
  ['sd', 'sd', ['200000'], {}, { console: ['HELLO'], fault: 'null' }],
  ['uart0', 'uart0', ['200000', '4096', '262144', '0', '0', '0', '72', '40000'], {},
    { console: ['uart0', "[rx 'H']"], fault: 'null' }],
  ['firmware-boot', 'firmware', ['2002944', '4096', '262144'], {},
    { console: ['/sd mounted', 'MicroPython', '>>>'], fault: 'null' }],
  ['bench', 'bench', ['200000'], {}, { console: ['benchmark'], fault: 'null' }],
  ['periphs', 'periphs', ['2000000'], {}, {
    console: ['RNG CTRL OK', 'Temperature OK', 'Clock Manager OK', 'I2S OK',
      'SPI1 ENABLES OK', 'USB GSNPSID OK', 'UART2-5 LSR OK', 'ALL PASS'],
    fault: 'null',
  }],
  ['debug', 'debug', ['500000'], {}, {
    console: ['debug/diagnostic', 'ALL PASS', 'Pass: 22'],
    fault: 'null',
  }],
];

let pass = 0, fail = 0, skip = 0;
for (const [name, prog, args, env, want] of CASES) {
  if (filter && !name.includes(filter)) { skip++; continue; }
  let L;
  try {
    L = run(prog, args, env);
  } catch (e) { console.log('FAIL', name, 'runner crashed'); fail++; continue; }
  const problems = [];
  const con = unesc(L.console || '');
  for (const s of want.console || []) {
    if (!con.includes(s)) problems.push(`console missing ${JSON.stringify(s)} (got ${JSON.stringify(con.slice(0, 100))})`);
  }
  if (want.fault === 'null') {
    const f = (L.meta || '').split(' ')[3];
    if (f !== 'null') problems.push(`fault: want null got ${f}`);
  } else if (want.fault) {
    const f = (L.meta || '').split(' ')[3] || '';
    if (!f.startsWith(want.fault)) problems.push(`fault: want ${want.fault} got ${f}`);
  }
  if (want.smp) {
    const got = (L.smp || '').split(' ').slice(0, 7); // drop trailing timing micros
    if (JSON.stringify(got) !== JSON.stringify(want.smp)) problems.push(`smp: want ${want.smp} got ${got}`);
  }
  if (want.fb) {
    if ((L.fb || '') !== want.fb) problems.push(`fb: want ${want.fb} got ${L.fb}`);
  }
  if (problems.length) { console.log('FAIL', name, '\n  ' + problems.join('\n  ')); fail++; }
  else { console.log('ok', name); pass++; }
}
console.log(`${pass} ok, ${fail} FAIL${skip ? `, ${skip} skipped` : ''}`);
process.exit(fail ? 1 : 0);
