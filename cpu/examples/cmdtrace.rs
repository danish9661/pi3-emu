use pi_cpu::{load_elf, Bus, Cpu};
fn main() {
    let bytes = std::fs::read("/home/danish1075/Documents/ri pi emu/public/programs/firmware.elf").unwrap();
    let mut bus = Bus::new();
    bus.vt_ips = 262144;
    let entry = load_elf(&mut bus, &bytes).expect("load elf");
    let mut cpu = Cpu::new(entry);
    let mut n = 0u64;
    while n < 268000 {
        bus.sync_out();
        let mut done = 0u64;
        while done < 4096 {
            if cpu.step(&mut bus).is_err() { println!("early FAULT at {}", n + done); return; }
            done += 1;
        }
        n += done;
        bus.sync_in(done);
    }
    for i in 0..12000 {
        eprintln!("T {} {:#x}", 268000 + i, cpu.pc);
        if cpu.step(&mut bus).is_err() { println!("FAULT"); return; }
    }
}
