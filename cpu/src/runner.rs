//! Shared run-loop for pi-cpu hosts (moved verbatim out of the
//! `run` example so the native differential runner and the wasm browser
//! core execute identical chunk/sync/IRQ logic).
//!
//! Chunk order mirrors the facade runSlice order (syncOut -> execute ->
//! syncIn -> IRQ_RET resume -> delivery), so host-assisted VBAR+0x280
//! entries land at the same guest points given the same chunk size.

use crate::{load_elf, Bus, Cpu, Fault};

pub const BTN_BIT: u32 = 1 << 29;

pub struct Runner {
    /// Total instruction budget (`run_to` stops here; wasm passes
    /// u64::MAX-ish targets per call and tracks `n` itself).
    pub budget: u64,
    /// Instructions per sync chunk (facade parity: 4096).
    pub slice: u64,
    /// Button schedule (insn counts, 0 = disabled). Drives guests with
    /// a polled-then-IRQ button phase (gpio).
    pub press1: u64,
    pub release1: u64,
    pub press2: u64,
    /// Single key push into the PL011 RX FIFO at an insn count
    /// (uart0's "type a key" phase).
    pub keybyte: u64,
    pub keyat: u64,
    pub key_done: bool,
    /// Multi-key schedule (insn, byte), sorted by time (firmware REPL
    /// sessions): edge-triggered pushes, like keybyte.
    pub keys: Vec<(u64, u8)>,
    /// Executed instruction count.
    pub n: u64,
    /// First fault (stops the run).
    pub fault: Option<Fault>,
    saved_pc: Option<u64>,
    saved_daif: u8,
    resume_armed: bool,
    vector_pending: Option<u64>,
}

impl Runner {
    pub fn new() -> Self {
        Runner {
            budget: u64::MAX,
            slice: 4096,
            press1: 0,
            release1: 0,
            press2: 0,
            keybyte: 0,
            keyat: 0,
            key_done: false,
            keys: Vec::new(),
            n: 0,
            fault: None,
            saved_pc: None,
            saved_daif: 0,
            resume_armed: false,
            vector_pending: None,
        }
    }

    /// Run until `n` reaches `target` (or fault). Returns instructions
    /// executed in this call.
    pub fn run_to(&mut self, cpu: &mut Cpu, bus: &mut Bus, target: u64) -> u64 {
        let start = self.n;
        while self.n < target && self.n < self.budget {
            if self.press1 != 0 && self.n >= self.press1 {
                bus.gpio_in |= BTN_BIT;
            }
            if self.release1 != 0 && self.n >= self.release1 {
                bus.gpio_in &= !BTN_BIT;
            }
            if self.press2 != 0 && self.n >= self.press2 {
                bus.gpio_in |= BTN_BIT;
            }
            if self.keybyte != 0 && self.keyat != 0 && !self.key_done && self.n >= self.keyat {
                bus.uart0_push((self.keybyte & 0xff) as u8);
                self.key_done = true;
            }
            while !self.keys.is_empty() && self.n >= self.keys[0].0 {
                bus.uart0_push(self.keys.remove(0).1);
            }
            bus.sync_out();
            // Actuation (facade runSlice start: irqResume || irqVector, both
            // cleared unconditionally once consumed-or-not).
            let do_resume = self.resume_armed;
            let vec = self.vector_pending.take();
            self.resume_armed = false;
            if do_resume {
                if let Some(sp) = self.saved_pc {
                    cpu.pc = sp;
                }
                // Host-assisted resume restores pre-entry DAIF (facade-
                // equivalent: the host never masked it).
                cpu.daif = self.saved_daif;
                self.saved_pc = None;
            } else if let Some(v) = vec {
                cpu.pc = v;
            }
            let m = core::cmp::min(self.slice, target - self.n);
            let m = core::cmp::min(m, self.budget - self.n);
            let mut done = 0u64;
            while done < m {
                if let Err(f) = cpu.step(bus) {
                    self.fault = Some(f);
                    if std::env::var("PI3_FAULTCTX").is_ok() {
                        eprintln!(
                            "FAULTCTX pc=0x{:x} x21=0x{:x} x8=0x{:x}",
                            cpu.pc, cpu.x[21], cpu.x[8]
                        );
                    }
                    break;
                }
                done += 1;
            }
            self.n += done;
            bus.sync_in(done);
            if self.fault.is_some() {
                break;
            }
            if bus.irq_ret_pending {
                bus.irq_ret_pending = false;
                // Resume actuates next pre-chunk. saved_pc/saved_daif already
                // hold the entry snapshot.
                self.resume_armed = true;
            }
            // Fresh decision at the CURRENT (end-of-chunk) pc — the facade's
            // irqElr. Delivery sources mirror the hardware/facade split: the
            // legacy GPU line (host line) plus the arch-timer gt condition
            // (owned by the core internally on the facade). cntp is disabled
            // for the legacy-IC guests, so they only see the GPU line. No
            // in-flight flag: entry masks DAIF, which blocks re-entry while
            // a handler runs (completion unmasks via magic or eret).
            if self.vector_pending.is_none()
                && !cpu.irq_masked()
                && (bus.legacy_line() || bus.cntp_line())
            {
                // Real exception entry snapshot at the CURRENT end-of-chunk
                // pc/PSTATE (= facade post-slice irqElr timing, which the
                // host-assisted resume path needs exactly). KNOWN RESIDUAL
                // for native-entry guests: the fork enters at chained-TB
                // granularity (a few insns into the slice), so ELR can lag
                // the architectural entry pc by a TB sliver (8B observed on
                // lirq phase B: x1 only; console/regs/pc/insns all match).
                // Reproducing chained-TB entry is out of scope (depends on
                // translator cache state, not the architecture).
                // DAIF masked, vector at VBAR+0x280. The IRQ_RET magic path
                // resumes host-assisted guests to saved_pc; eret resumes the
                // rest natively at ELR.
                cpu.elr_el1 = cpu.pc;
                cpu.spsr_el1 = cpu.pstate();
                self.saved_daif = cpu.daif;
                cpu.daif = 0xf;
                self.saved_pc = Some(cpu.pc);
                let vbar = if cpu.vbar_el1 == 0 { 0x100000 } else { cpu.vbar_el1 };
                self.vector_pending = Some(vbar + 0x280);
            }
        }
        self.n - start
    }

    /// Fault as a short string (wasm boundary + diff output).
    pub fn fault_string(&self) -> Option<String> {
        self.fault.as_ref().map(|f| format!("{:?}", f))
    }
}

/// Shared SMP mailbox arbiter (mirrors main.js smpState + the probe's
/// state): the only state shared between cores; mirrored per core per
/// chunk by [`SmpShared::sync_out`] / [`sync_in`] (exact ports of
/// syncDeviceOut/syncDeviceIn, same order and conditions).
#[derive(Clone, Copy)]
pub struct SmpShared {
    pub go: u32,
    pub counter: u32,
    pub lock: u32,
    pub park: u32,
    pub msg: [u32; 4],
    pub start: [u32; 3],
}

impl SmpShared {
    pub fn new() -> Self {
        SmpShared {
            go: 0,
            counter: 0,
            lock: 0,
            park: 0,
            msg: [0; 4],
            start: [0; 3],
        }
    }

    /// Push the arbiter's commit state into the core's window before it
    /// runs (the device the core sees IS the commit state).
    pub fn sync_out(&self, bus: &mut Bus, core: usize) {
        bus.smp_write32(0x38, core as u32); // CPUID
        bus.smp_write32(0x30, core as u32); // CURRENT
        bus.smp_write32(0x10, self.go);
        bus.smp_write32(0x14, self.counter);
        bus.smp_write32(0x18, self.lock);
        bus.smp_write32(0x34, self.park);
        for i in 0..4 {
            bus.smp_write32(0x1c + i as u64 * 4, self.msg[i]);
        }
    }

    /// Pull whatever the core wrote back into shared state (commit after
    /// each chunk). START_ENTRY/GO come from core 0 only; counter/lock
    /// are last-writer-wins in core order; MSG latches first-nonzero;
    /// PARK accumulates.
    pub fn sync_in(&mut self, bus: &Bus, core: usize) {
        if core == 0 {
            for k in 0..3 {
                let v = bus.smp_read32((k + 1) as u64 * 4);
                if v != 0 && self.start[k] == 0 {
                    self.start[k] = v;
                }
            }
            if bus.smp_read32(0x10) != 0 {
                self.go = 1;
            }
        }
        let ctr = bus.smp_read32(0x14);
        if ctr != self.counter {
            self.counter = ctr;
        }
        let lk = bus.smp_read32(0x18);
        if lk != self.lock {
            self.lock = lk;
        }
        if self.msg[core] == 0 {
            self.msg[core] = bus.smp_read32(0x1c + core as u64 * 4);
        }
        self.park |= bus.smp_read32(0x34);
    }
}

/// One SMP core: private CPU + private Bus (partitioned RAM, like the
/// facade's per-core instances; only the mailbox is shared).
pub struct SmpCore {
    pub cpu: Cpu,
    pub bus: Bus,
}

/// Round-robin 4-core scheduler (mirrors main.js smpRun): core 0 starts
/// at the ELF entry; cores 1..3 start at their START_ENTRY address once
/// core 0 publishes it; parked cores are skipped; stops when all park
/// (or on first fault / round cap).
pub struct SmpRunner {
    pub cores: Vec<SmpCore>,
    pub shared: SmpShared,
    pub entries: [u64; 4],
    pub started: [bool; 4],
    pub console: Vec<u8>,
    pub fault: Option<Fault>,
    pub total: u64,
    pub rounds: u64,
}

impl SmpRunner {
    pub fn new(elf: &[u8]) -> Result<Self, String> {
        let mut cores = Vec::new();
        let mut entry0 = 0u64;
        for i in 0..4 {
            let mut bus = Bus::new();
            let entry = load_elf(&mut bus, elf)?;
            if i == 0 {
                entry0 = entry;
            }
            // Secondaries start with pc 0 (like a fresh unicorn whose PC
            // reads 0): the scheduler sets pc to entries[i] once core 0
            // publishes START_ENTRY, so they enter at smp_coreN with
            // their own stacks — never at _start.
            cores.push(SmpCore {
                cpu: Cpu::new(if i == 0 { entry } else { 0 }),
                bus,
            });
        }
        Ok(SmpRunner {
            cores,
            shared: SmpShared::new(),
            entries: [entry0, 0, 0, 0],
            started: [true, false, false, false],
            console: Vec::new(),
            fault: None,
            total: 0,
            rounds: 0,
        })
    }

    /// Run round-robin slices until all cores park, `max_rounds`
    /// rounds, `budget` total insns, or first fault.
    pub fn run(&mut self, slice: u64, max_rounds: u64, budget: u64) {
        const ALL_PARKED: u32 = (1 << 4) - 1;
        'outer: for _ in 0..max_rounds {
            self.rounds += 1;
            for i in 0..4 {
                if !self.started[i] {
                    let e = self.shared.start[i - 1];
                    if e == 0 {
                        continue;
                    }
                    self.started[i] = true;
                    self.entries[i] = e as u64;
                }
                if self.shared.park & (1 << i) != 0 {
                    continue;
                }
                // Mirror smpRun: pc persists per core; a fresh core starts
                // at its published entry.
                let core = &mut self.cores[i];
                if core.cpu.pc == 0 {
                    core.cpu.pc = self.entries[i];
                }
                self.shared.sync_out(&mut core.bus, i);
                let mut done = 0u64;
                while done < slice && self.total < budget {
                    if let Err(f) = core.cpu.step(&mut core.bus) {
                        self.fault = Some(f);
                        break 'outer;
                    }
                    done += 1;
                    self.total += 1;
                }
                self.shared.sync_in(&core.bus, i);
                self.console.extend_from_slice(&core.bus.console);
                core.bus.console.clear();
                if self.shared.park == ALL_PARKED {
                    break 'outer;
                }
            }
        }
    }

    /// Fault as a short string.
    pub fn fault_string(&self) -> Option<String> {
        self.fault.as_ref().map(|f| format!("{:?}", f))
    }
}
