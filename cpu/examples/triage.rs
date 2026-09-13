// M56 triage runner: argv DTB_END KERN_END BUDGET SLICE. Slices the
// committed .data in-process (no 26MB hex pipe), loads via load_linux(),
// resets per ARM64 boot protocol, runs Runner chunks, prints first fault.
use pi_cpu::{load_linux, runner::Runner, Bus, Cpu, LINUX_DTB_PA};

fn esc(s: &[u8]) -> String {
    let mut o = String::new();
    for &b in s.iter().take(300) {
        match b {
            b'\\' => o.push_str("\\\\"),
            b'"' => o.push_str("\\\""),
            b'\n' => o.push_str("\\n"),
            b'\r' => o.push_str("\\r"),
            b'\t' => o.push_str("\\t"),
            c if (0x20..0x7f).contains(&c) => o.push(c as char),
            c => o.push_str(&format!("\\u{:04x}", c)),
        }
    }
    o
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dtb_end: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(32753);
    let kern_end: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(22505969);
    let budget: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(200000);
    let slice: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(4096);
    let data = std::fs::read("public/linux/qemu-system-aarch64.data").expect("read .data");
    let (dtb, rest) = data.split_at(dtb_end);
    let (kernel, initrd) = rest.split_at(kern_end - dtb_end);
    let mut bus = Bus::new();
    let entry = load_linux(&mut bus, kernel, dtb, initrd).expect("load_linux");
    println!("triage\tentry=0x{:x} ram=0x{:x} kernel={} dtb={} initrd={}",
        entry, bus.ram_size(), kernel.len(), dtb.len(), initrd.len());
    let mut cpu = Cpu::new(entry);
    // ARM64 boot protocol: x0=DTB PA, EL2, MMU off, SP high in RAM.
    cpu.linux_reset(entry, LINUX_DTB_PA, 0x1FFF_FFF0);
    let mut runner = Runner::new();
    runner.budget = budget;
    runner.slice = slice;
    bus.vt_ips = 262144;
    runner.run_to(&mut cpu, &mut bus, budget);
    println!("triage\tn={} pc=0x{:x} x0=0x{:x} fault={} console={:?}",
        runner.n, cpu.pc, cpu.x[0],
        runner.fault_string().unwrap_or_else(|| "null".into()),
        esc(&bus.console));
}
