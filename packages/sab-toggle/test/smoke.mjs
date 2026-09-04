import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import vm from 'node:vm';

const __dirname = dirname(fileURLToPath(import.meta.url));
const src = readFileSync(join(__dirname, '..', 'sab-toggle.js'), 'utf8');

let pass = 0;
function check(name, cond, extra = '') {
  if (cond) { pass++; console.log('ok', name); }
  else { console.log('FAIL', name, extra); process.exitCode = 1; }
}

// Load the classic script into a fresh browser-ish sandbox (vm contexts lack
// URL/URLSearchParams, which all real browsers and plain node provide).
function load(win = {}) {
  const sandbox = { window: win, location: win.location || { search: '', hash: '' },
    URLSearchParams, URL };
  sandbox.window = sandbox.window || {};
  sandbox.globalThis = sandbox;
  vm.createContext(sandbox);
  vm.runInContext(src + '\n;this.SabToggle;', sandbox);
  return sandbox.window.SabToggle || vm.runInContext('SabToggle', sandbox);
}

// 1. No-window (node-like) environment never throws.
const T0 = load({});
check('api surface', ['detect', 'getPreference', 'setPreference', 'decide', 'resolve',
  'ensureIsolation', 'probeFile', 'pickVariant', 'bindSelect', 'describe']
  .every((f) => typeof T0[f] === 'function'));
const d0 = T0.detect();
check('node detect shape', d0.supported === false && typeof d0.reason === 'string', JSON.stringify(d0));
check('decide on -> multi', T0.decide('on').effective === 'multi');
check('decide off -> single', T0.decide('off').effective === 'single');
check('decide garbage -> auto', T0.decide('bogus').requested === 'auto');

// 2. Preference order: explicit > ?threads= > storage > auto.
const store = {};
const T1 = load({ location: { search: '?threads=off', hash: '' }, localStorage: {
  getItem: (k) => store[k] ?? null, setItem: (k, v) => { store[k] = v; } } });
check('query beats default', T1.getPreference() === 'off');
check('explicit beats query', T1.getPreference('on') === 'on');
check('setPreference persists', T1.setPreference('on') === 'on' && store['sab.threads'] === 'on');

// 3. Capable browser resolves auto -> multi with reason.
const T2 = load({ crossOriginIsolated: true, isSecureContext: true,
  location: { search: '', hash: '' } });
T2.__sab = true;
const realSAB = globalThis.SharedArrayBuffer;
const r = T2.resolve();
check('resolve shape', r.requested === 'auto' && typeof r.reason === 'string');
void realSAB;

console.log(pass + ' checks run');
