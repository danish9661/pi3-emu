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

/// M117: the Linux path runs at the real Pi 3 instruction rate (one
/// 19.2 MHz tick per instruction ~= 50 ns). Override with
/// PI3_VTIPS=<n>; the bare-metal default used to be 262144, which
/// left the arch timer chronically overdue.
fn linux_ips() -> u64 {
    std::env::var("PI3_VTIPS").ok().and_then(|v| v.parse().ok()).unwrap_or(26214400)
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
    bus.vt_ips = linux_ips();
    // PI3_KEY=<byte>: push that byte into the PL011 RX FIFO every 20 M
    // instructions. busybox init's `askfirst` handler waits for a key
    // before it runs `/bin/sh`, so without this the boot stops at the
    // console prompt and the shell is never exercised.
    let key = std::env::var("PI3_KEY").ok().map(|k| k.as_bytes()[0]);
    // PI3_STUCK=<chunks>: dump the register file + memory around
    // x0/x1/x22 when the pc has been unchanged for that many chunks.
    let stuck_k: Option<u64> = std::env::var("PI3_STUCK").ok().and_then(|v| v.parse().ok());
    // PI3_SAMPLE_EVERY=N: print n/pc/x0/x30 every N insns (trajectory to
    // find where progress stops; default 0 = only the final line).
    let every: u64 = std::env::var("PI3_SAMPLE_EVERY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if every == 0 {
        // With a key schedule the loop must stay chunked so the byte is
        // pushed while init is actually waiting at the console.
        if key.is_some() {
            // PI3_SCANPA=<hex>: poll a PA every chunk and log when it
            // changes, with the pc — catches writes that bypass every
            // instrumented path (the ash trap[] phantom at 0x13ce678).
            let scanpa: u64 = std::env::var("PI3_SCANPA").ok()
                .and_then(|v| u64::from_str_radix(v.trim_start_matches("0x"), 16).ok()).unwrap_or(0);
            let mut scan_last = if scanpa != 0 { bus.mem_u64_dbg_pub(scanpa) } else { 0 };
            while runner.n < budget && runner.fault_string().is_none() {
                if runner.n % 20_000_000 < 4096 {
                    bus.uart0_push(key.unwrap());
                }
                runner.run_to(&mut cpu, &mut bus, (runner.n + 4096).min(budget));
                if scanpa != 0 {
                    let cur = bus.mem_u64_dbg_pub(scanpa);
                    if cur != scan_last {
                        eprintln!("SCANPA pa=0x{:x} 0x{:x} -> 0x{:x} n={} pc=0x{:x}", scanpa, scan_last, cur, runner.n, cpu.pc);
                        scan_last = cur;
                    }
                }
            }
        } else {
            runner.run_to(&mut cpu, &mut bus, budget);
        }
    } else {
        let mut done = 0u64;
        while done < budget && runner.fault_string().is_none() {
            let target = (done + every).min(budget);
            runner.run_to(&mut cpu, &mut bus, target);
            done = runner.n;
            println!("sample\tn={} pc=0x{:x} x0=0x{:x} x30=0x{:x} fault={}",
                runner.n, cpu.pc, cpu.x[0], cpu.x[30],
                runner.fault_string().unwrap_or_else(|| "null".into()));
            if runner.n < target {
                break;
            }
        }
    }
    // PI3_STUCK=<chunks>: while the chunked loop runs, report when the pc
    // has not moved for N chunks — the "where is it spinning?" answer,
    // with the register file and the bytes around x0/x1 so a string scan
    // or a pointer walk can be judged by reading the dump.
    if let (Some(k), true) = (stuck_k, key.is_some() || stuck_k.is_some()) {
        let mut same = 0u64;
        let mut last = cpu.pc >> 9;
        while runner.n < budget && runner.fault_string().is_none() {
            if runner.n % 20_000_000 < 4096 {
                if let Some(kb) = key {
                    bus.uart0_push(kb);
                }
            }
            runner.run_to(&mut cpu, &mut bus, (runner.n + 4096).min(budget));
            // Granularity matters: a tight loop cycles through several
            // pcs, so compare the pc's 512-byte page, not the pc.
            let here = cpu.pc >> 9;
            if here == last {
                same += 1;
            } else {
                same = 0;
                last = here;
            }
            if same == k {
                println!(
                    "stuck\tn={} pc=0x{:x} x0=0x{:x} x1=0x{:x} x2=0x{:x} x3=0x{:x} sp=0x{:x} x19=0x{:x} x20=0x{:x} x21=0x{:x} x22=0x{:x} el={}",
                    runner.n, cpu.pc, cpu.x[0], cpu.x[1], cpu.x[2], cpu.x[3], cpu.sp,
                    cpu.x[19], cpu.x[20], cpu.x[21], cpu.x[22], cpu.cur_el
                );
                for (label, va) in [("x0", cpu.x[0]), ("x1", cpu.x[1]), ("x22", cpu.x[22])] {
                    if va < (1 << 48) {
                        if let Ok(pa) = bus.translate(va & !0xf) {
                            let mut bytes = String::new();
                            for i in 0..32u64 {
                                bytes.push_str(&format!("{:02x}", bus.mem_u32_dbg(pa + i) as u8));
                            }
                            println!("stuckmem\t{}=0x{:x} pa=0x{:x} {}", label, va, pa, bytes);
                        }
                    }
                }
                break;
            }
        }
    }
    println!("triage\tn={} pc=0x{:x} x0=0x{:x} fault={} console={:?}",
        runner.n, cpu.pc, cpu.x[0],
        runner.fault_string().unwrap_or_else(|| "null".into()),
        esc(&bus.console));
    // Milestone markers, so `test/linux-triage.mjs` doubles as the
    // "how far does the boot get" check instead of needing a
    // throwaway probe. Counts are over the whole console; the tail is
    // the last few lines with the per-second noise filtered out.
    {
        let c = String::from_utf8_lossy(&bus.console);
        println!("markers\tbytes={} bb_len={:.2}", c.len(), runner.steps as f64 / runner.jumps.max(1) as f64);
        for m in [
            "Run /bin/init",
            "VFS: Mounted root",
            "EXT4-fs",
            "Please press Enter",
            "starting interactive shell",
            "~ #",
            "Segmentation fault",
            "Kernel panic",
            "malloc",
            "err -110",
            "REGISTER DUMP",
        ] {
            println!("marker\t{:<26} {}", m, c.matches(m).count());
        }
        let noise = |l: &str| l.contains("thermal_zone0") || l.contains("mmc1: Timeout");
        let lines: Vec<&str> = c
            .split(|ch| ch == '\n' || ch == '\r')
            .filter(|l| !l.is_empty() && !noise(l))
            .collect();
        for l in lines.iter().skip(lines.len().saturating_sub(8)) {
            println!("tail\t{}", &l[..l.len().min(120)]);
        }
    }
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
        bus2.vt_ips = linux_ips();
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
