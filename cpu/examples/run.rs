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
use pi_cpu::{load_elf, Bus, Cpu};
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
    let mut key_done = false;
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
        keys.sort();
    }
    const BTN: u32 = 1 << 29;
    let bytes = std::fs::read(path).expect("read elf");
    let mut bus = Bus::new();
    bus.vt_ips = vt_ips;
    let entry = load_elf(&mut bus, &bytes).expect("load elf");
    let mut cpu = Cpu::new(entry);
    let t0 = Instant::now();
    // Chunk loop mirroring the facade runSlice order (syncOut -> execute
    // -> syncIn -> IRQ_RET resume -> delivery), so host-assisted VBAR+0x280
    // entries land at the same guest points given the same chunk size.
    let mut n = 0u64;
    let mut fault = None;
    let mut saved_pc: Option<u64> = None;
    let mut saved_daif: u8 = 0;
    // Post-chunk decision state, mirroring the facade's post-slice
    // syncIrqRet (irqResume) + irqDeliver (irqVector), both consumed at
    // the next chunk start (facade runSlice start: irqResume || irqVector,
    // resume wins).
    let mut resume_armed = false;
    let mut vector_pending: Option<u64> = None;
    while n < budget {
        if press1 != 0 && n >= press1 {
            bus.gpio_in |= BTN;
        }
        if release1 != 0 && n >= release1 {
            bus.gpio_in &= !BTN;
        }
        if press2 != 0 && n >= press2 {
            bus.gpio_in |= BTN;
        }
        if keybyte != 0 && keyat != 0 && !key_done && n >= keyat {
            bus.uart0_push((keybyte & 0xff) as u8);
            key_done = true;
        }
        while !keys.is_empty() && n >= keys[0].0 {
            bus.uart0_push(keys.remove(0).1);
        }
        bus.sync_out();
        // Actuation (facade runSlice start: irqResume || irqVector, both
        // cleared unconditionally once consumed-or-not).
        let do_resume = resume_armed;
        let vec = vector_pending.take();
        resume_armed = false;
        if do_resume {
            if let Some(sp) = saved_pc {
                cpu.pc = sp;
            }
            // Host-assisted resume restores pre-entry DAIF (facade-
            // equivalent: the host never masked it).
            cpu.daif = saved_daif;
            saved_pc = None;
        } else if let Some(v) = vec {
            cpu.pc = v;
        }
        let m = core::cmp::min(slice, budget - n);
        let mut done = 0u64;
        while done < m {
            if let Err(f) = cpu.step(&mut bus) {
                fault = Some(f);
                break;
            }
            done += 1;
        }
        n += done;
        bus.sync_in(done);
        if fault.is_some() {
            break;
        }
        if bus.irq_ret_pending {
            bus.irq_ret_pending = false;
            // Resume actuates next pre-chunk. saved_pc/saved_daif already
            // hold the entry snapshot.
            resume_armed = true;
        }
        // Fresh decision at the CURRENT (end-of-chunk) pc — the facade's
        // irqElr. Delivery sources mirror the hardware/facade split: the
        // legacy GPU line (host line) plus the arch-timer gt condition
        // (owned by the core internally on the facade). cntp is disabled
        // for the legacy-IC guests, so they only see the GPU line. No
        // in-flight flag: entry masks DAIF, which blocks re-entry while
        // a handler runs (completion unmasks via magic or eret).
        if vector_pending.is_none()
            && !cpu.irq_masked()
            && (bus.legacy_line() || bus.cntp_line())
        {
            // Real exception entry snapshot at the CURRENT end-of-chunk
            // pc/PSTATE (= facade post-slice irqElr timing, which the
            // host-assisted resume path needs exactly). KNOWN RESIDUAL
            // for native-entry guests: the fork enters at chained-TB
            // granularity (a few insns into the slice), so ELR can lag
            // the architectural entry pc by a TB sliver (8B observed on
            // lirq phase B: x1 only; console/regs/pc/insns all match).
            // Reproducing chained-TB entry is out of scope (depends on
            // translator cache state, not the architecture).
            // DAIF masked, vector at VBAR+0x280. The IRQ_RET magic path
            // resumes host-assisted guests to saved_pc; eret resumes the
            // rest natively at ELR.
            cpu.elr_el1 = cpu.pc;
            cpu.spsr_el1 = cpu.pstate();
            saved_daif = cpu.daif;
            cpu.daif = 0xf;
            saved_pc = Some(cpu.pc);
            let vbar = if cpu.vbar_el1 == 0 { 0x100000 } else { cpu.vbar_el1 };
            vector_pending = Some(vbar + 0x280);
        }
    }
    let us = t0.elapsed().as_micros();
    println!("console\t{}", esc(&bus.console));
    let regs: Vec<String> = cpu.x.iter().map(|r| r.to_string()).collect();
    println!("regs\t{}", regs.join(" "));
    println!(
        "meta\t{} {} {} {} {}",
        cpu.sp,
        cpu.pc,
        n,
        match fault {
            None => "null".to_string(),
            Some(f) => format!("{:?}", f),
        },
        us
    );
}
