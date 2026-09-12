// Single-instruction differential probe for test/cpu-cases.mjs:
// argv: WORD x0..x30 sp nzcv memseed? Executes exactly one step from
// pc=0x100 (word placed there; scratch pattern at 0x1000) and prints
// regs/nzcv/fault/memdump lines for comparison against the unicorn oracle.
use pi_cpu::{Bus, Cpu};

fn hex(s: &str) -> u64 {
    u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let word = hex(&args[1]) as u32;
    let mut bus = Bus::new();
    bus.write(0x100, 4, word as u64).unwrap();
    // Scratch patterns both sides share (test/cpu-cases.mjs writes the same).
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
    // FP seeds (args 35..66): D-reg bit patterns (fuzzer compares D0-D31).
    // Full 128-bit Q patterns also accepted (hex up to 32 digits) for
    // vector-row verification (test/simd-oracle.mjs); shorter values
    // seed the low half exactly like before.
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
    match cpu.step(&mut bus) {
        Ok(()) => println!("status ok"),
        Err(e) => println!("status fault {:?}", e),
    }
    let regs: Vec<String> = cpu.x.iter().map(|r| format!("{:016x}", r)).collect();
    println!("regs {}", regs.join(" "));
    let fpregs: Vec<String> = cpu.q.iter().map(|r| format!("{:016x}", (*r as u64))).collect();
    println!("fpregs {}", fpregs.join(" "));
    let qregs: Vec<String> = cpu.q.iter().map(|r| format!("{:032x}", r)).collect();
    println!("qregs {}", qregs.join(" "));
    println!("sp {:016x} pc {:x}", cpu.sp, cpu.pc);
    let fl = cpu.flags();
    println!(
        "nzcv {}{}{}{}",
        fl.0 as u8, fl.1 as u8, fl.2 as u8, fl.3 as u8
    );
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
