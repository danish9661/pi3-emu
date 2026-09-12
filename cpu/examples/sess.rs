// Interactive session driver for the ported headless suites
// (test/upython-*.mjs, formerly driven via the unicorn Pi3Emulator
// facade). A persistent subprocess speaking lines on stdin/stdout so one
// guest session survives across many drip-fed keys and slice polls:
//
//   LOAD <elfpath>   fresh Bus/Cpu/Runner (vt_ips=10M, facade VIRTUAL_IPS
//                    pace, so guest sleeps complete while slices run)
//   RUN <n>          run n more insns (4096-chunks, facade slice parity)
//   KEY <byte>       push one PL011 RX byte
//   BTN <0|1>        set the GPIO BTN29 input level
//   VT <ips>         set virtual-time rate (0 = wall clock)
//   TICK <us>        wall_tick (wall-clock mode only)
//   READ <addr>      u32 MMIO/RAM peek (like the facade's readU32)
//   CARDOUT          export the SD image as hex
//   CARDIN <hex>     replace the SD image (before boot output matters)
//   EXIT             quit
//
// Every command answers exactly one line (JSON string values quoted with
// serde-style minimal escaping done by hand — no dependencies).
use pi_cpu::{load_elf, runner::Runner, Bus, Cpu};
use std::io::{BufRead, Write};

fn jstr(s: &[u8]) -> String {
    let mut o = String::from("\"");
    for &b in s {
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
    o.push('"');
    o
}

fn hexbytes(s: &str) -> Vec<u8> {
    let s = s.trim();
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap_or(0))
        .collect()
}

fn tohex(b: &[u8]) -> String {
    let mut o = String::with_capacity(b.len() * 2);
    for &x in b {
        o.push(char::from_digit((x >> 4) as u32, 16).unwrap());
        o.push(char::from_digit((x & 15) as u32, 16).unwrap());
    }
    o
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    let mut bus = Bus::new();
    let mut cpu = Cpu::new(0);
    let mut runner = Runner::new();
    let mut n: u64 = 0;
    // Card image staged by CARDIN before the next LOAD ("import before
    // boot"): LOAD resets the Bus (fresh disk), so the image is kept
    // aside and applied after the ELF loads.
    let mut pending_card: Option<Vec<u8>> = None;
    for line in stdin.lock().lines() {
        let line = line.unwrap_or_default();
        let mut it = line.splitn(2, ' ');
        let cmd = it.next().unwrap_or("");
        let arg = it.next().unwrap_or("").trim();
        match cmd {
            "LOAD" => {
                bus = Bus::new();
                bus.vt_ips = 10_000_000; // facade VIRTUAL_IPS pace
                match std::fs::read(arg) {
                    Ok(bytes) => match load_elf(&mut bus, &bytes) {
                        Ok(entry) => {
                            cpu = Cpu::new(entry);
                            runner = Runner::new();
                            runner.slice = 4096;
                            n = 0;
                            if let Some(img) = pending_card.take() {
                                bus.sd_import(&img);
                            }
                            writeln!(out, "OK entry {:#x}", entry).unwrap();
                        }
                        Err(e) => writeln!(out, "ERR {}", e).unwrap(),
                    },
                    Err(e) => writeln!(out, "ERR {}", e).unwrap(),
                }
            }
            "RUN" => {
                let want: u64 = arg.parse().unwrap_or(0);
                let target = n.saturating_add(want);
                runner.budget = u64::MAX;
                runner.run_to(&mut cpu, &mut bus, target);
                n = runner.n;
                let con = core::mem::take(&mut bus.console);
                let f = runner.fault_string().unwrap_or_else(|| "null".into());
                writeln!(out, "OK con {} fault {} n {}", jstr(&con), f, n).unwrap();
            }
            "KEY" => {
                let b: u8 = arg.parse().unwrap_or(0);
                bus.uart0_push(b);
                writeln!(out, "OK").unwrap();
            }
            "BTN" => {
                if arg == "1" {
                    bus.gpio_in |= 1 << 29;
                } else {
                    bus.gpio_in &= !(1 << 29);
                }
                writeln!(out, "OK").unwrap();
            }
            "VT" => {
                bus.vt_ips = arg.parse().unwrap_or(0);
                writeln!(out, "OK").unwrap();
            }
            "TICK" => {
                bus.wall_tick(arg.parse().unwrap_or(0));
                writeln!(out, "OK").unwrap();
            }
            "READ" => {
                let a: u64 = if arg.starts_with("0x") || arg.starts_with("0X") {
                    u64::from_str_radix(arg.trim_start_matches(['0', 'x', 'X']), 16).unwrap_or(0)
                } else {
                    arg.parse().unwrap_or(0)
                };
                // Device-aware peek (GPIO FSEL/LEV); RAM reads directly.
                let v = if Bus::is_ram(a, 4) {
                    let b = bus.mem_read_bytes(a, 4);
                    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
                } else {
                    bus.peek(a)
                };
                writeln!(out, "OK {}", v).unwrap();
            }
            "CARDOUT" => {
                writeln!(out, "OK {}", tohex(&bus.sd_export())).unwrap();
            }
            "CARDIN" => {
                let img = hexbytes(arg);
                let ok = bus.sd_import(&img);
                if ok {
                    pending_card = Some(img);
                }
                writeln!(out, "OK {}", ok).unwrap();
            }
            "EXIT" => {
                writeln!(out, "OK").unwrap();
                out.flush().unwrap();
                return;
            }
            _ => {
                writeln!(out, "ERR unknown").unwrap();
            }
        }
        out.flush().unwrap();
    }
}
