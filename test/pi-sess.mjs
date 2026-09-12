// pi-cpu session helper for the headless suites (replaces the unicorn
// Pi3Emulator facade): spawns cpu/examples/sess (release) once per suite
// and mirrors the facade surface the suites use (runSlice/pushKey/
// consoleText/lastFault/setButton/readU32/exportCard/importCard plus
// no-op attach* — pi-cpu's Bus always has every window attached).
//
// Async (await every runSlice/READ/CARD call): replies arrive over the
// stdout pipe. pushKey/setButton are fire-and-forget stdin writes — pipe
// ordering guarantees they land before any later RUN.
import { spawn } from 'node:child_process';
import { writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const ROOT = join(__dirname, '..');
export const SESS_BIN = join(ROOT, 'target', 'release', 'examples', 'sess');

// Reap sess children when the suite exits (suites never close sessions
// explicitly; without this node lingers on the open pipes after PASS).
const liveProcs = new Set();
process.once('exit', () => {
  for (const p of liveProcs) { try { p.kill('SIGKILL'); } catch (_) {} }
});

export class PiSess {
  constructor() {
    this.consoleText = '';
    this.lastFault = null;
    this.proc = null;
    this.pending = '';
    this.waiters = [];
    this.dead = false;
  }
  _onData(d) {
    this.pending += d.toString('utf8');
    for (;;) {
      const nl = this.pending.indexOf('\n');
      if (nl < 0) break;
      const line = this.pending.slice(0, nl);
      this.pending = this.pending.slice(nl + 1);
      const w = this.waiters.shift();
      if (w) w(line);
      // null waiter = fire-and-forget reply, discarded in order.
    }
  }
  _cmd(line, timeoutMs = 240000) {
    // Lazy spawn: importCard (and friends) legitimately run before the
    // first loadFirmware ("import before boot"), so ensure the child.
    if (!this.proc && !this.dead) this._spawnNow();
    if (this.dead || !this.proc) throw new Error('sess is dead');
    if (process.env.PI_SESS_LOG) console.error(`sess> ${line.slice(0, 80)}`);
    return new Promise((res, rej) => {
      const tag = line.slice(0, 12);
      const to = setTimeout(() => {
        rej(new Error(`sess timeout (${timeoutMs}ms) on: ${tag}...`));
      }, timeoutMs);
      if (to.unref) to.unref();
      this.waiters.push((l) => {
        clearTimeout(to);
        if (process.env.PI_SESS_LOG) console.error(`sess< ${String(l).slice(0, 120)}`);
        res(l);
      });
      try {
        this.proc.stdin.write(line + '\n', (e) => { if (e) rej(e); });
      } catch (e) { rej(e); }
    });
  }
  _fire(line) {
    // Queue a discard waiter: sess replies OK to every line, and replies
    // arrive in order — without consuming it here, the next _cmd would
    // read this OK as its own reply.
    if (!this.proc && !this.dead) this._spawnNow();
    if (!this.dead && this.proc) {
      try {
        this.waiters.push(null);
        this.proc.stdin.write(line + '\n');
      } catch (_) {}
    }
  }
  _spawnNow() {
    this.proc = spawn(SESS_BIN, [], { stdio: ['pipe', 'pipe', 'ignore'] });
    liveProcs.add(this.proc);
    // Child death must fail loudly: settle every pending waiter with an
    // error line (RUN/READ/CARD parsers throw on it) instead of hanging.
    const bury = () => {
      this.dead = true;
      liveProcs.delete(this.proc);
      while (this.waiters.length) {
        const w = this.waiters.shift();
        try { if (w) w('ERR sess-dead'); } catch (_) {}
      }
    };
    this.proc.on('error', bury);
    this.proc.on('exit', bury);
    this.proc.stdout.on('data', (d) => this._onData(d));
  }
  async _loadPath(f) {
    if (!this.proc && !this.dead) this._spawnNow();
    const r = await this._cmd(`LOAD ${f}`);
    if (!r.startsWith('OK')) throw new Error('sess LOAD failed: ' + r);
  }
  async loadFirmware(bytes) {
    const f = join(tmpdir(), `pi-sess-${process.pid}.elf`);
    writeFileSync(f, Buffer.from(bytes));
    this.consoleText = '';
    this.lastFault = null;
    await this._loadPath(f);
    return 0;
  }
  async runSlice(n = 4096) {
    const r = await this._cmd(`RUN ${n}`);
    // OK con <json> fault <f|null> n <total>
    const m = r.match(/^OK con (".*") fault (\S+) n (\d+)$/);
    if (!m) throw new Error('sess RUN bad reply: ' + r.slice(0, 160));
    this.consoleText += JSON.parse(m[1]);
    this.lastFault = m[2] === 'null' ? null : { message: m[2] };
  }
  pushKey(code) { this._fire(`KEY ${code & 0xff}`); }
  sendLine(s) {
    for (const ch of String(s)) this.pushKey(ch.charCodeAt(0));
    this.pushKey(13);
  }
  setButton(down) { this._fire(down ? 'BTN 1' : 'BTN 0'); }
  async readU32(addr) {
    const r = await this._cmd(`READ ${addr}`);
    const m = r.match(/^OK (\d+)$/);
    if (!m) throw new Error('sess READ bad reply: ' + r);
    return Number(m[1]) >>> 0;
  }
  async exportCard() {
    const r = await this._cmd('CARDOUT');
    const m = r.match(/^OK ([0-9a-f]*)$/);
    if (!m) throw new Error('sess CARDOUT bad reply');
    return Buffer.from(m[1], 'hex');
  }
  async importCard(bytes) {
    const r = await this._cmd(`CARDIN ${Buffer.from(bytes).toString('hex')}`);
    return /^OK true/.test(r);
  }
  // No-ops: pi-cpu's Bus always has every window attached.
  attachSdhci() { return {}; }
  attachUart1() { return {}; }
  attachI2c() { return {}; }
  attachSpi() { return {}; }
  attachPwm() { return {}; }
  attachDma() { return {}; }
  attachMmu() { return {}; }
  close() { try { liveProcs.delete(this.proc); this.proc.kill(); } catch (_) {} }
}
