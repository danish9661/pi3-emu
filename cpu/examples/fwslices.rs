use pi_cpu::{load_elf, Bus, Cpu};
fn main() {
    let bytes = std::fs::read("/home/danish1075/Documents/ri pi emu/public/programs/firmware.elf").unwrap();
    let mut bus = Bus::new();
    bus.vt_ips = 262144;
    let entry = load_elf(&mut bus, &bytes).expect("load elf");
    let mut cpu = Cpu::new(entry);
    let mut n = 0u64;
    while n < 228000 {
        bus.sync_out();
        let mut done = 0u64;
        while done < 4096 {
            if cpu.step(&mut bus).is_err() { println!("FAULT at {}", n + done); return; }
            done += 1;
        }
        n += done;
        bus.sync_in(done);
        println!("{} 0x{:x} {}", n, cpu.pc, cpu.x.iter().take(10).map(|r| (r & 0xffffffff).to_string()).collect::<Vec<_>>().join(" "));
    }
}
