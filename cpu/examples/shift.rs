// 2-source differential probe: single LSLV/LSRV/ASRV/RORV/UDIV/SDIV
// words (32- and 64-bit) with fixed operands; prints `op sf result`.
// Compared against the unicorn oracle by test/cpu-shift.mjs.
use pi_cpu::{Bus, Cpu};

// op2 values from assembler truth (see M42 notes): LSLV=0b1000,
// LSRV=0b1001, ASRV=0b1010, RORV=0b1011, UDIV=0b10, SDIV=0b11.
fn main() {
    // (op2, sf, rn_val, rm_val)
    let cases: Vec<(u32, bool, u64, u64)> = vec![
        (0b1000, true, 1, 21),
        (0b1000, false, 0x12345678, 9),
        (0b1001, true, 0x123456789abcdef0, 13),
        (0b1001, false, 0x12345678, 4),
        (0b1010, true, 0x8000000000000001, 1),
        (0b1010, false, 0x80000001, 1),
        (0b1011, true, 0x123456789abcdef0, 8),
        (0b1011, false, 0x12345678, 8),
        (0b0010, true, 100, 7),
        (0b0010, false, 100, 7),
        (0b0010, true, 100, 0),
        (0b0011, true, 100, 7),
        (0b0011, false, 100, 7),
        (0b0011, true, 100, 0),
        (0b0011, true, 0x8000000000000000, 0xffffffffffffffff),
    ];
    for (op2, sf, a, b) in cases {
        let base: u32 = if sf { 0x9ac00000 } else { 0x1ac00000 };
        let word = base | (5 << 16) | (op2 << 10) | (4 << 5) | 3;
        let mut bus = Bus::new();
        bus.write(0x100, 4, word as u64).unwrap();
        let mut cpu = Cpu::new(0x100);
        cpu.x[4] = a;
        cpu.x[5] = b;
        match cpu.step(&mut bus) {
            Ok(()) => println!("{} {} {:016x}", op2, sf as u8, cpu.x[3]),
            Err(f) => println!("{} {} FAULT {:?}", op2, sf as u8, f),
        }
    }
}
