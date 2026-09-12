// Differential runner: loads a guest ELF, executes a fixed instruction
// budget, and prints tab-separated lines (console-escaped, regs,
// meta) for test/cpu-diff.mjs, which assembles the comparison JSON.
// NOTE (M40): keep these prints small and separate — during bring-up, a
// big single println! with many formatted args intermittently correlated
// with a wrong x28 low half (CF90/D000 vs CCCD) in ways that never
// reproduced in manual-loop mains or in-process A/Bs and never survived
// `rm -rf target` rebuilds; treated as build/observation flakiness, NOT
// lib semantics (the lib is proven by single-step probes + watch traces
// + full shell agreement). If x28 ever mismatches again, rebuild from
// `rm -rf target` before theorizing.
//
// The chunk loop lives in pi_cpu::runner (shared verbatim with the wasm
// browser core) so native and browser behavior match by construction.
use pi_cpu::{load_elf, runner::Runner, runner::SmpRunner, Bus, Cpu};
use std::time::Instant;

fn esc(s: &[u8]) -> String {
    let mut o = String::new();
    for &b in s {
        match b {
            b'\\' => o.push_str("\\\\"),
            b'"' => o.push_str("\\\""),
            b'\n' => o.push_str("\\n"),
            b'\r' => o.push_str("\\r"),
            b'\t' => o.push_str("\\t"),
            c if (0x20..0x7f).contains(&c) => o.push(c as char),
            c => o.push_str(&format!("\\u{:04x}", c)),
        }
    }
    o
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: run <elf> [budget] [slice] [vt_ips] [press1] [release1] [press2] [keybyte] [keyat]");
    let budget: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(200000);
    let slice: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4096);
    let vt_ips: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
    // Optional host button schedule (insn counts, 0 = disabled): press the
    // BTN29 input, release it, press again (left held). Drives guests with
    // a polled-then-IRQ button phase (gpio).
    let press1: u64 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(0);
    let release1: u64 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(0);
    let press2: u64 = args.get(7).and_then(|s| s.parse().ok()).unwrap_or(0);
    // Optional single key push into the PL011 RX FIFO at an insn count
    // (uart0's "type a key" phase).
    let keybyte: u64 = args.get(8).and_then(|s| s.parse().ok()).unwrap_or(0);
    let keyat: u64 = args.get(9).and_then(|s| s.parse().ok()).unwrap_or(0);
    // Optional multi-key schedule PI3_KEYS="insn:byte,insn:byte,..."
    // (firmware REPL sessions): edge-triggered pushes, like keybyte.
    let mut keys: Vec<(u64, u8)> = Vec::new();
    if let Ok(spec) = std::env::var("PI3_KEYS") {
        for part in spec.split(',') {
            let mut it = part.split(':');
            if let (Some(a), Some(b)) = (it.next(), it.next()) {
                if let (Ok(at), Ok(byte)) = (a.parse::<u64>(), b.parse::<u8>()) {
                    keys.push((at, byte));
                }
            }
        }
        // Stable by time only: tuple sort would reorder same-count keys
        // by byte value (briefly shipped that way: \r,+,1,1).
        keys.sort_by_key(|k| k.0);
    }
    let bytes = std::fs::read(path).expect("read elf");
    // SMP quad mode (PI3_SMP=1): 4 partitioned cores + shared mailbox,
    // round-robin slices (see SmpRunner). No IRQs in this guest (polling
    // only; DAIF stays masked), so the plain chunk loop suffices.
    if std::env::var("PI3_SMP").is_ok() {
        let t0 = Instant::now();
        let mut smp = SmpRunner::new(&bytes).expect("load elf");
        smp.run(slice, 2000, budget);
        let us = t0.elapsed().as_micros();
        println!("console\t{}", esc(&smp.console));
        println!(
            "smp\t{} {} {} {} {} {} {} {}",
            smp.shared.park,
            smp.shared.counter,
            smp.shared.msg[0],
            smp.shared.msg[1],
            smp.shared.msg[2],
            smp.shared.msg[3],
            smp.fault_string().unwrap_or_else(|| "null".to_string()),
            us
        );
        return;
    }
    let mut bus = Bus::new();
    bus.vt_ips = vt_ips;
    let entry = load_elf(&mut bus, &bytes).expect("load elf");
    let mut cpu = Cpu::new(entry);
    let mut runner = Runner::new();
    runner.budget = budget;
    runner.slice = slice;
    runner.press1 = press1;
    runner.release1 = release1;
    runner.press2 = press2;
    runner.keybyte = keybyte;
    runner.keyat = keyat;
    runner.keys = keys;
    let t0 = Instant::now();
    runner.run_to(&mut cpu, &mut bus, budget);
    let us = t0.elapsed().as_micros();
    println!("console\t{}", esc(&bus.console));
    let regs: Vec<String> = cpu.x.iter().map(|r| r.to_string()).collect();
    println!("regs\t{}", regs.join(" "));
    // Framebuffer età (only when the guest allocated one via mailbox):
    // FNV-1a over the pixels + corner/center samples, so the smoke test
    // can pin the drawn pattern without dumping 76 KB (deterministic
    // under virtual time + fixed budget).
    let (fw, fh, fp, fready) = bus.fb_geometry();
    if fready {
        let frame = bus.mem_read_bytes(0x200000, (fw as usize) * (fh as usize) * 4);
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in &frame {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        let px = |x: usize, y: usize| -> u32 {
            let o = (y * fp as usize + x * 4).min(frame.len().saturating_sub(4));
            u32::from_le_bytes(frame[o..o + 4].try_into().unwrap())
        };
        println!(
            "fb\t{}x{} p{} hash{:016x} c0={:08x} c1={:08x} c2={:08x} cc={:08x}",
            fw,
            fh,
            fp,
            h,
            px(0, 0),
            px(fw as usize - 1, fh as usize - 1),
            px(2, 2),
            px(fw as usize / 2, fh as usize / 2)
        );
    }
    println!(
        "meta\t{} {} {} {} {}",
        cpu.sp,
        cpu.pc,
        runner.n,
        runner.fault_string().unwrap_or_else(|| "null".to_string()),
        us
    );
}
