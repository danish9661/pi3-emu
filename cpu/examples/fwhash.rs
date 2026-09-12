use pi_cpu::{load_elf, Bus, Cpu};
fn main() {
    let bytes = std::fs::read("/home/danish1075/Documents/ri pi emu/public/programs/firmware.elf").unwrap();
    let mut bus = Bus::new();
    bus.vt_ips = 262144;
    let entry = load_elf(&mut bus, &bytes).expect("load elf");
    let mut cpu = Cpu::new(entry);
    let mut n = 0u64;
    let step: u64 = 1024;
    while n < 285000 {
        let m = core::cmp::min(step, 231000 - n);
        bus.sync_out();
        let mut done = 0u64;
        while done < m {
            if cpu.step(&mut bus).is_err() { println!("FAULT at {}", n + done); return; }
            done += 1;
        }
        n += done;
        bus.sync_in(done);
        // hash RAM + key regs
        let mut h = 0xcbf29ce484222325u64;
        // (access RAM via reads in 4KB strides for speed + full low regions)
        println!("{} {:#x} {:#x} {} {} {}", n, cpu.pc, bus.mem_hash(), cpu.x[0] & 0xffffffff, cpu.x[1] & 0xffffffff, cpu.sp);
    }
}
