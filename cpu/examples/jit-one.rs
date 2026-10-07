// M118b JIT-oracle: EXACTLY one.rs's argv/output interface, but executes
// the snippet via the Cranelift JIT (a 1-insn block) instead of the
// interpreter. Used by `node test/cpu-cases.mjs --jit` to diff the JIT's
// register/flag/memory state against the interpreter's goldens — the
// full-fuzzer hardening pass for kernel safety.
//
// argv: WORD x0..x30 sp nzcv memseed?
// status: `ok` (ran), `bail` (not compilable -> interpreter fallback OK),
//         `fault <F>` (same shape as one.rs).

use pi_cpu::{Bus, Cpu};

fn hex(s: &str) -> u64 {
    u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let word = hex(&args[1]) as u32;
    let mut bus = Bus::new();
    bus.write(0x100, 4, word as u64).unwrap();
    for i in 0..16u64 {
        let w = 0x01020304u64.wrapping_add(i * 0x11111111);
        bus.write(0x1000 + i * 4, 4, w).unwrap();
    }
    for i in 0..8u64 {
        let w = 0x01020304u64.wrapping_add(i * 0x11111111);
        bus.write(0x2000 + i * 4, 4, w).unwrap();
    }
    for i in 0..16u64 {
        let w = 0x01020304u64.wrapping_add(i * 0x11111111);
        bus.write(0x3fff00 - 64 + i * 4, 4, w).unwrap();
    }
    let mut cpu = Cpu::new(0x100);
    for i in 0..31 {
        cpu.x[i] = hex(&args[2 + i]);
    }
    for i in 0..32 {
        if let Some(s) = args.get(35 + i) {
            let t = s.trim_start_matches("0x");
            cpu.q[i] = u128::from_str_radix(t, 16).unwrap();
        }
    }
    cpu.sp = hex(&args[33]);
    let f = &args[34];
    let fb: Vec<bool> = f.chars().map(|c| c == '1').collect();
    cpu.set_flags(fb[0], fb[1], fb[2], fb[3]);

    let mut jit = pi_cpu::jit::Jit::new();
    let f2 = jit.compile(&mut cpu, 0x100, &[word]);
    match f2 {
        None => println!("status bail"),
        Some(f) => {
            let (n, z, c, v) = cpu.flags();
            let mut nzcv = [n as u8, z as u8, c as u8, v as u8];
            let next = unsafe {
                f(&mut bus, cpu.x.as_mut_ptr(), &mut cpu.sp, nzcv.as_mut_ptr())
            };
            if next < 0 {
                println!("status fault UnmappedData");
                // Match the interpreter's pre-incremented fault pc.
                cpu.pc = 0x104;
            } else {
                println!("status ok");
                cpu.pc = next as u64;
            }
            cpu.set_flags(nzcv[0] != 0, nzcv[1] != 0, nzcv[2] != 0, nzcv[3] != 0);
        }
    }
    let regs: Vec<String> = cpu.x.iter().map(|r| format!("{:016x}", r)).collect();
    println!("regs {}", regs.join(" "));
    let fpregs: Vec<String> = cpu.q.iter().map(|r| format!("{:016x}", (*r as u64))).collect();
    println!("fpregs {}", fpregs.join(" "));
    let qregs: Vec<String> = cpu.q.iter().map(|r| format!("{:032x}", r)).collect();
    println!("qregs {}", qregs.join(" "));
    println!("sp {:016x} pc {:x}", cpu.sp, cpu.pc);
    let fl = cpu.flags();
    println!("nzcv {}{}{}{}", fl.0 as u8, fl.1 as u8, fl.2 as u8, fl.3 as u8);
    let mut wins = Vec::new();
    for (base, len) in [(0x1000u64, 64u64), (0x2000, 32), (0x3fff00 - 64, 64)] {
        let mut h = String::new();
        for i in 0..len {
            h.push_str(&format!("{:02x}", bus.read(base + i, 1).unwrap_or(0xdd)));
        }
        wins.push(h);
    }
    println!("mem {}", wins.join(" "));
}
