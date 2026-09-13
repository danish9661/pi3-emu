// M56 triage runner: argv DTB_END KERN_END BUDGET SLICE. Slices the
// committed .data in-process (no 26MB hex pipe), loads via load_linux(),
// resets per ARM64 boot protocol, runs Runner chunks, prints first fault.
// M57: PI3_TRACE=1 dumps the post-MMU instruction trace (pc + ESR/TCR/
// TTBRs + faulting VA + page-walk), so the next gap is root-caused by
// execution, never by reading.
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
    // M57 post-MMU trace: re-run step-by-step from reset and dump the
    // last K insns before the fault (pc + raw word + ESR/TCR/TTBRs),
    // plus a page-walk of the faulting VA. PI3_TRACE=1 enables.
    if std::env::var("PI3_TRACE").is_ok() {
        let n = runner.n;
        let start = n.saturating_sub(40);
        let mut bus2 = Bus::new();
        let entry2 = load_linux(&mut bus2, kernel, dtb, initrd).expect("reload");
        let mut cpu2 = Cpu::new(entry2);
        cpu2.linux_reset(entry2, LINUX_DTB_PA, 0x1FFF_FFF0);
        bus2.vt_ips = 262144;
        let mut r2 = Runner::new();
        r2.budget = start;
        r2.slice = slice;
        r2.run_to(&mut cpu2, &mut bus2, start);
        println!("trace\treplay to n={} pc=0x{:x}", start, cpu2.pc);
        // Decode the faulting VA from the runner fault string
        // (UnmappedData(addr) / Translation(addr) carry the PA-or-VA).
        let fstr = runner.fault_string().unwrap_or_default();
        let fva: u64 = fstr
            .trim_start_matches("UnmappedData(")
            .trim_start_matches("Translation(")
            .trim_end_matches(')')
            .parse()
            .unwrap_or(0xffffffc008ba1aa8);
        for i in start..n {
            let pc = cpu2.pc;
            let word = bus2.fetch(pc).unwrap_or(0xdead_c0de);
            let va_info = if (cpu2.pc >> 55) & 1 != 0 { "hi" } else { "lo" };
            println!("trace\t+{} pc=0x{:x} w=0x{:08x} {} sctlr=0x{:x} tcr=0x{:x} ttbr0=0x{:x} ttbr1=0x{:x}",
                i, pc, word, va_info, bus2.mmu_sctlr, bus2.mmu_tcr, bus2.mmu_ttbr0, bus2.mmu_ttbr1);
            if cpu2.step(&mut bus2).is_err() {
                println!("trace\tfault at step {} (expected {})", i, n);
                break;
            }
        }
        // Page-walk dump of the faulting VA at the fault point.
        println!("trace\twalk va=0x{:x}: {}", fva, bus2.walk_dump(fva));
        // Direct PA probe: is the walk's output PA actually in RAM?
        // (Separates walk-math bugs from missing-window bugs.)
        if let Ok(pa) = bus2.translate(fva) {
            println!("trace\ttranslate(va)=0x{:x} in_ram={}", pa, bus2.in_ram(pa, 8));
        } else {
            println!("trace\ttranslate(va) FAULTS");
        }
    }
}
