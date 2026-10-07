//! M118 JIT spike benchmark: the honest usage pattern.
//!
//! A tight compute+memory loop (assembled, never hand-hex) is run through
//! (a) the plain interpreter and (b) a TB runner that compiles each
//! straight-line block once with the Cranelift JIT and re-enters it on
//! every backward branch. Both run the same total instructions; the ratio
//! is the honest answer to "does codegen help on a real loop?"
//!
//! TRIAGE SPIKE — delete or promote after measurement.

use pi_cpu::{Bus, Cpu};
use std::time::Instant;

// The loop from loop.s (assembled, verified):
//   movz x4,#0 ; movz x5,#0 ; mov x7,x2
// L: ldr x6,[x7] ; add x4,x4,x6 ; add x7,x7,#8 ; add x5,x5,#1 ; cmp x5,x3 ; b.lo L
//    mov x0,x4 ; ret
const LOOP: [u32; 11] = [
    0xd280_0004, 0xd280_0005, 0xaa02_03e7, 0xf940_00e6, 0x8b06_0084, 0x9100_20e7,
    0x9100_04a5, 0xeb03_00bf, 0x54ff_ff63, 0xaa04_03e0, 0xd65f_03c0,
];
const LOOP_PC: u64 = 0x100000;
const BLOCK_START: u64 = 0x10000c; // `L:` — the loop body
const RET_PC: u64 = 0x1ffff0;

fn seed(cpu: &mut Cpu, bus: &mut Bus, n: u64) {
    cpu.x[2] = 0x200000;
    cpu.x[3] = n;
    for i in 0..n {
        bus.write(0x200000 + i * 8, 8, i).unwrap();
    }
    cpu.x[30] = RET_PC;
    cpu.x[4] = 0;
    cpu.x[5] = 0;
}

fn block_words(bus: &mut Bus, pc: u64) -> [u32; 8] {
    let mut w = [0u32; 8];
    for i in 0..8 {
        w[i] = bus.read(pc + i as u64 * 4, 4).unwrap_or(0) as u32;
    }
    w
}

fn main() {
    const INNER: u64 = 32;
    const REPS: u64 = 300_000;
    let mut bus = Bus::new();
    for (i, w) in LOOP.iter().enumerate() {
        bus.write(LOOP_PC + i as u64 * 4, 4, *w as u64).unwrap();
    }

    // ---------- plain interpreter ----------
    let mut cpu = Cpu::new(LOOP_PC);
    seed(&mut cpu, &mut bus, INNER);
    let t0 = Instant::now();
    let mut steps = 0u64;
    for _ in 0..REPS {
        cpu.pc = LOOP_PC;
        cpu.x[4] = 0;
        cpu.x[5] = 0;
        while cpu.pc != RET_PC {
            cpu.step(&mut bus).unwrap();
            steps += 1;
        }
    }
    let dt = t0.elapsed().as_secs_f64();
    println!(
        "interp: {} steps in {:.2}s = {:.1} MIPS (x0=0x{:x})",
        steps, dt, steps as f64 / dt / 1e6, cpu.x[0]
    );

    // ---------- TB runner with the Cranelift JIT ----------
    let mut cpu = Cpu::new(LOOP_PC);
    seed(&mut cpu, &mut bus, INNER);
    let mut jit = pi_cpu::jit::Jit::new();
    let mut nzcv = [0u8; 4];
    let t0 = Instant::now();
    let mut steps = 0u64;
    let mut calls = 0u64;
    for _ in 0..REPS {
        cpu.pc = LOOP_PC;
        cpu.x[4] = 0;
        cpu.x[5] = 0;
        // Prologue (3 insns) via the interpreter to reach the loop body.
        for _ in 0..3 {
            cpu.step(&mut bus).unwrap();
            steps += 1;
        }
        // The loop: one compiled block per entry pc, re-entered on the
        // backward branch (bb_len ~6 per entry + the tail).
        loop {
            let pc = cpu.pc;
            if pc == RET_PC {
                break;
            }
            let words = block_words(&mut bus, pc);
            let f = match jit.compile(&mut cpu, pc, &words) {
                Some(f) => f,
                None => {
                    // Unsupported: run it interpreted.
                    cpu.step(&mut bus).unwrap();
                    steps += 1;
                    continue;
                }
            };
            let next = unsafe {
                f(&mut bus, cpu.x.as_mut_ptr(), &mut cpu.sp, nzcv.as_mut_ptr())
            } as u64;
            calls += 1;
            steps += 6; // block body = 6 insns (loop.s L:..b.lo)
            if calls < 40 {
                println!("  call {}: pc=0x{:x} -> 0x{:x} x5={} x3={} x30=0x{:x}", calls, pc, next, cpu.x[5], cpu.x[3], cpu.x[30]);
            }
            if next as i64 == -1 {
                break;
            }
            cpu.pc = next;
        }
    }
    let dt = t0.elapsed().as_secs_f64();
    println!(
        "jit:    {} steps in {:.2}s = {:.1} MIPS (x0=0x{:x}, compiled={}, calls={})",
        steps,
        dt,
        steps as f64 / dt / 1e6,
        cpu.x[0],
        jit.compiled,
        calls
    );
}
