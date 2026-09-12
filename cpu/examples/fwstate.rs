use pi_cpu::{load_elf, Bus, Cpu};
fn main() {
    let budget: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(160000);
    let bytes = std::fs::read("/home/danish1075/Documents/ri pi emu/public/programs/firmware.elf").unwrap();
    let mut bus = Bus::new();
    bus.vt_ips = 262144;
    let entry = load_elf(&mut bus, &bytes).expect("load elf");
    let mut cpu = Cpu::new(entry);
    let mut n = 0u64;
    while n < budget {
        bus.sync_out();
        let mut done = 0u64;
        while done < 4096 {
            if cpu.step(&mut bus).is_err() { println!("FAULT"); return; }
            done += 1;
        }
        n += done;
        bus.sync_in(done);
    }
    println!("memhash={:#x} pc={:#x}", bus.mem_hash(), cpu.pc);
    println!("regs={}", cpu.x.iter().take(29).map(|r| (r & 0xffffffff).to_string()).collect::<Vec<_>>().join(" "));
}
