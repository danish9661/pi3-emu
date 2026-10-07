//! M118b JIT hardening: run each snippet through BOTH the interpreter
//! and the Cranelift JIT, and diff the full state (x0-x30, sp, NZCV,
//! a scratch memory window). Finds the mis-compiled arms that hang the
//! kernel boot. Snippets are assembled with aarch64-none-elf-as (never
//! hand-hex).
//!
//! TRIAGE SPIKE — delete or promote after measurement.

use pi_cpu::{Bus, Cpu};

struct Case {
    name: &'static str,
    word: u32,
}

// A curated set over the JIT's arm subset, assembled+verified.
const CASES: &[Case] = &[
    Case { name: "movz", word: 0xd2800d24 }, // movz x4, #105
    Case { name: "movz64", word: 0xd2abfac5 }, // movz x5, #0x5fd6, lsl #16
    Case { name: "movk", word: 0xf2becfa4 }, // movk x4, #0xf67d, lsl #48
    Case { name: "mov-reg", word: 0xaa0203e7 }, // mov x7, x2
    Case { name: "mvn", word: 0xaa2203e1 }, // mvn x1, x2
    Case { name: "add-imm", word: 0x910020e7 }, // add x7, x7, #8
    Case { name: "sub-imm", word: 0xd10020e7 }, // sub x7, x7, #8
    Case { name: "adds-imm", word: 0xb10020e7 }, // adds x7, x7, #8
    Case { name: "subs-imm", word: 0xf10020e7 }, // subs x7, x7, #8
    Case { name: "add-reg", word: 0x8b060084 }, // add x4, x4, x6
    Case { name: "sub-reg", word: 0xcb060084 }, // sub x4, x4, x6
    Case { name: "adds-reg", word: 0xab060084 }, // adds x4, x4, x6
    Case { name: "subs-reg", word: 0xeb060084 }, // subs x4, x4, x6 (cmp shape)
    Case { name: "and", word: 0x8a030041 }, // and x1, x2, x3
    Case { name: "orr", word: 0xaa030041 }, // orr x1, x2, x3
    Case { name: "eor", word: 0xca030041 }, // eor x1, x2, x3
    Case { name: "ands", word: 0xea030041 }, // ands x1, x2, x3
    Case { name: "bic", word: 0x8a230041 }, // bic x1, x2, x3
    Case { name: "lsl", word: 0xd34df846 }, // lsl x6, x2, #13
    Case { name: "lsr", word: 0xd34dfc46 }, // lsr x6, x2, #13
    Case { name: "asr", word: 0x934dfc46 }, // asr x6, x2, #13
    Case { name: "cmp-imm", word: 0xf10020bf }, // cmp x5, #8
    Case { name: "mul", word: 0x9b037c44 }, // mul x4, x2, x3
    Case { name: "madd", word: 0x9b030844 }, // madd x4, x2, x3, x2
    Case { name: "msub", word: 0x9b038844 }, // msub x4, x2, x3, x2
    Case { name: "umulh", word: 0x9bc37c44 }, // umulh x4, x2, x3
    Case { name: "udiv", word: 0x9ac30844 }, // udiv x4, x2, x3
    Case { name: "csel", word: 0x9a821040 }, // csel x0, x2, x2, eq
    Case { name: "cset", word: 0x9a9f17e0 }, // cset x0, ne
    Case { name: "ldr-uoff", word: 0xf94000e6 }, // ldr x6, [x7]
    Case { name: "ldr-off", word: 0xf94004e6 }, // ldr x6, [x7, #8]
    Case { name: "str-uoff", word: 0xf90000e6 }, // str x6, [x7]
    Case { name: "ldrb", word: 0x394000e6 }, // ldrb w6, [x7]
    Case { name: "strb", word: 0x390000e6 }, // strb w6, [x7]
    Case { name: "ldrh", word: 0x794000e6 }, // ldrh w6, [x7]
    Case { name: "strh", word: 0x790000e6 }, // strh w6, [x7]
    Case { name: "ldp", word: 0xa9400ce6 }, // ldp x6, x3, [x7]
    Case { name: "stp", word: 0xa9000ce6 }, // stp x6, x3, [x7]
    Case { name: "adr", word: 0x10001500 }, // adr x0, <+0x2a0>
    Case { name: "adrp", word: 0x90000016 }, // adrp x22, <page>
    Case { name: "ldur", word: 0xf85f00e6 }, // ldur x6, [x7, #-16]
    Case { name: "stur", word: 0xf81f00e6 }, // stur x6, [x7, #-16]
];

fn seed(cpu: &mut Cpu, bus: &mut Bus) {
    for i in 0..31 {
        cpu.x[i] = 0x01020304u64.wrapping_add((i as u64) * 0x11111111);
    }
    cpu.x[7] = 0x200000; // memory base for ld/st
    cpu.x[2] = 0x200000;
    cpu.sp = 0x3ffff0;
    for i in 0..16u64 {
        bus.write(0x200000 + i * 8, 8, 0x0102030405060708u64.wrapping_add(i)).unwrap();
    }
    cpu.set_flags(true, false, true, false);
}

fn state(cpu: &Cpu, bus: &mut Bus) -> String {
    let mut s = String::new();
    for i in 0..31 {
        s.push_str(&format!("x{}={:016x} ", i, cpu.x[i]));
    }
    s.push_str(&format!("sp={:016x} ", cpu.sp));
    let f = cpu.flags();
    s.push_str(&format!("nzcv={}{}{}{} ", f.0 as u8, f.1 as u8, f.2 as u8, f.3 as u8));
    for i in 0..4u64 {
        s.push_str(&format!("m{:x}={:x} ", i, bus.read(0x200000 + i * 8, 8).unwrap_or(0)));
    }
    s
}

fn main() {
    let mut fails = 0;
    for c in CASES {
        // interpreter
        let mut bus = Bus::new();
        bus.write(0x100000, 4, c.word as u64).unwrap();
        let mut cpu = Cpu::new(0x100000);
        seed(&mut cpu, &mut bus);
        cpu.step(&mut bus).unwrap();
        let want = state(&cpu, &mut bus);
        // JIT
        let mut bus = Bus::new();
        bus.write(0x100000, 4, c.word as u64).unwrap();
        let mut cpu = Cpu::new(0x100000);
        seed(&mut cpu, &mut bus);
        let mut jit = pi_cpu::jit::Jit::new();
        let (fn_, fz, fc, fv) = cpu.flags();
        let mut nzcv = [fn_ as u8, fz as u8, fc as u8, fv as u8];
        let words = [c.word];
        match jit.compile(&mut cpu, 0x100000, &words) {
            Some(f) => {
                let _ = unsafe { f(&mut bus, cpu.x.as_mut_ptr(), &mut cpu.sp, nzcv.as_mut_ptr()) };
                if ["ands","msub","umulh"].contains(&c.name) { println!("    jit nzcv={:?} x4=0x{:x} x1=0x{:x}", nzcv, cpu.x[4], cpu.x[1]); }
                cpu.set_flags(nzcv[0] != 0, nzcv[1] != 0, nzcv[2] != 0, nzcv[3] != 0);
            }
            None => {
                println!("  {:<10} COMPILE-BAIL", c.name);
                continue;
            }
        }
        let got = state(&cpu, &mut bus);
        if ["ands","msub","umulh"].contains(&c.name) { println!("    after set_flags cpu.flags={:?}", cpu.flags()); }
        if want != got {
            fails += 1;
            println!("  {:<10} DIFF", c.name);
            println!("    want {}", want);
            println!("    got  {}", got);
        }
    }
    println!("jit-diff fails={}", fails);
    if fails > 0 {
        std::process::exit(1);
    }
}
