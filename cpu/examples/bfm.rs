// BFM differential probe: executes single BFM words (64- and 32-bit,
// S<R and S>=R shapes) with fixed src/dst and prints `R S sf result`.
// Compared against the unicorn oracle by test/cpu-bfm.mjs.
use pi_cpu::{Bus, Cpu};

fn main() {
    // (R, S, sf, src, dst)
    let cases: Vec<(u32, u32, bool, u64, u64)> = vec![
        // 64-bit, S<R -> wmask form
        (28, 7, true, 0x123456789abcdef0, 0x0fedcba987654321),
        (13, 5, true, 0x123456789abcdef0, 0x0fedcba987654321),
        (52, 32, true, 0x123456789abcdef0, 0x0fedcba987654321),
        (60, 7, true, 0xdeadbeefcafef00d, 0x0123456789abcdef),
        // 64-bit, S>=R -> tmask form
        (4, 11, true, 0x123456789abcdef0, 0x0fedcba987654321),
        (8, 40, true, 0x123456789abcdef0, 0x0fedcba987654321),
        (2, 60, true, 0x123456789abcdef0, 0x0fedcba987654321),
        (10, 10, true, 0x123456789abcdef0, 0x0fedcba987654321),
        (0, 63, true, 0x123456789abcdef0, 0x0fedcba987654321),
        // canonical aliases: BFI x3,x1,#4,#8 ; BFXIL x3,x1,#4,#8 ; UBFM/SBFM-adjacent
        // 32-bit shapes
        (10, 4, false, 0x12345678, 0x0fedcba9),
        (4, 11, false, 0x12345678, 0x0fedcba9),
        (0, 31, false, 0x12345678, 0x0fedcba9),
        (20, 15, false, 0xdeadbeef, 0x01234567),
        (7, 23, false, 0xdeadbeef, 0x01234567),
    ];
    for (r, s, sf, src, dst) in cases {
        let word: u32 = if sf {
            0xb3400000 | (r << 16) | (s << 10) | 0x23
        } else {
            0x33000000 | (r << 16) | (s << 10) | 0x23
        };
        let mut bus = Bus::new();
        bus.write(0x100, 4, word as u64).unwrap();
        let mut cpu = Cpu::new(0x100);
        cpu.x[1] = src;
        cpu.x[3] = dst;
        match cpu.step(&mut bus) {
            Ok(()) => println!("{} {} {} {:016x}", r, s, sf as u8, cpu.x[3]),
            Err(f) => println!("{} {} {} FAULT {:?}", r, s, sf as u8, f),
        }
    }
}
