// M117: stage-1 walk + TLB differential (`cargo run --example
// mmu-tlb-diff`). Builds synthetic page tables and compares translate()
// against hand-computed PAs cold / on a TLB hit / after evicting the
// direct-mapped slot — 4K pages, 2M and 1G blocks, the high half with
// the same VPN as a low mapping. Exits non-zero on any mismatch.
//
// Run: node test/mmu-tlb.mjs
import { execFileSync } from 'node:child_process';

const EXAMPLE = 'mmu-tlb-diff';

try {
  execFileSync('cargo', ['build', '--release', '--example', EXAMPLE], {
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const out = execFileSync(`./target/release/examples/${EXAMPLE}`, { encoding: 'utf8' });
  process.stdout.write(out);
  const m = out.match(/fails=(\d+)/);
  const fails = m ? Number(m[1]) : 1;
  if (fails === 0) {
    console.log(`mmu-tlb: ${out.trim().split('\n').length - 1} cases ok, 0 FAIL`);
  } else {
    console.error(`mmu-tlb: ${fails} FAIL`);
    process.exit(1);
  }
} catch (e) {
  console.error('mmu-tlb: build/run failed');
  console.error(String(e.stdout || '') + String(e.stderr || ''));
  process.exit(1);
}
