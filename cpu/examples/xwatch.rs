use pi_cpu::{load_elf, Bus, Cpu};
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage");
    let bytes = std::fs::read(path).expect("read elf");
    let mut bus = Bus::new();
    bus.vt_ips = 262144;
    let entry = load_elf(&mut bus, &bytes).expect("load elf");
    let mut cpu = Cpu::new(entry);
    for n in 0..2000u64 {
        if cpu.pc == 0x1002cc {
            println!("step {} strb: x11={:#x} x9={} w13={:#x}", n, cpu.x[11], cpu.x[9], cpu.x[13] & 0xffffffff);
            cpu.step(&mut bus).unwrap();
            let a = cpu.x[11] + 19;
            println!("  after: mem[{:#x}]={:#x} (want 0x30)", a, bus.read(a, 1).unwrap());
            return;
        }
        if cpu.step(&mut bus).is_err() { println!("fault at {}", n); return; }
    }
}
