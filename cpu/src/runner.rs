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
    /// IRQ deliveries taken (M61 triage: proves whether the line ever
    /// delivered — a live line with zero deliveries = masked waiter).
    pub irqs: u64,
    /// PCs of the first 8 IRQ deliveries (M61 triage: proves WHICH line
    /// delivered — timer vector vs mailbox vector land at different
    /// handlers; elr shows the interrupted site).
    pub irq_pcs: Vec<u64>,
    /// M61 mailbox-IRQ drain log (MBOXTAG-gated): each entry is the `n`
    /// at which a chunk boundary observed the mailbox completing line
    /// de-asserted after having been live (mbox0=2 observed, then
    /// mbox0=0 at a later chunk). Proves the chained handler drained
    /// MAIL0_RD by execution. Grows only when `Bus::mbox_drain_log`
    /// is set; zero-cost otherwise.
    pub mbox_drains: Vec<u64>,
    /// M61 send-side watch (MBOXTAG-gated): last MAIL1 word observed,
    /// so the sender pc/DAIF at each MBOXWR is proven by execution.
    /// Zero-cost unless MBOXTAG is set (checked once per chunk).
    mbox_last_seen: u32,
    /// M61 DAIF watch (MBOXTAG-gated): last DAIF observed, so mask
    /// transitions while the mailbox line is live are proven.
    mbox_daif_last: u8,
    saved_pc: Option<u64>,
    saved_daif: u8,
    resume_armed: bool,
    vector_pending: Option<u64>,
    /// M118 block-length census: a counter per instruction and one per pc
    /// jump (branch/exception — where a TB must end). steps/jumps ≈ mean
    /// basic-block length, which decides whether a TB/JIT can amortize
    /// fetch+decode. Zero-cost (two u64 increments on the step loop).
    pub steps: u64,
    pub jumps: u64,
    /// M118b Cranelift JIT (native only, env `PI3_JIT`): hot backward-
    /// branch blocks compiled to native code. Fallback to the interpreter
    /// for unsupported blocks and on any boundary. Off by default.
    #[cfg(not(target_arch = "wasm32"))]
    pub jit: Option<crate::jit::Jit>,
    #[cfg(not(target_arch = "wasm32"))]
    jit_nzcv: [u8; 4],
    #[cfg(not(target_arch = "wasm32"))]
    jit_on: bool,
    #[cfg(not(target_arch = "wasm32"))]
    pub jit_boundary: bool,
    #[cfg(not(target_arch = "wasm32"))]
    jit_hits: [u8; 4096],
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
            irqs: 0,
            irq_pcs: Vec::new(),
            mbox_drains: Vec::new(),
            mbox_last_seen: 0,
            mbox_daif_last: 0xf,
            saved_pc: None,
            saved_daif: 0,
            resume_armed: false,
            steps: 0,
            jumps: 0,
            vector_pending: None,
            #[cfg(not(target_arch = "wasm32"))]
            jit: None,
            #[cfg(not(target_arch = "wasm32"))]
            jit_nzcv: [0; 4],
            #[cfg(not(target_arch = "wasm32"))]
            jit_on: std::env::var("PI3_JIT").is_ok(),
            #[cfg(not(target_arch = "wasm32"))]
            jit_boundary: false,
            #[cfg(not(target_arch = "wasm32"))]
            jit_hits: [0; 4096],
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
            // M76 storm-watch chunk tag: the sdhost model snapshots
            // these when the CMD13 storm declares (Bus has no pc/n).
            bus.sdh_chunk_n = self.n;
            bus.exc_n = self.n;
            bus.sdh_chunk_pc = cpu.pc;
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
                // M98 IRQ ENTRY (replaces the M95b guarded restore,
                // deleted): IRQ vectors are always EL1h — entry sets
                // cur_el=1 and the kernel stack is live by construction
                // (banks). No value copying. Bare-metal guests are
                // EL1-only: cur_el 1→1 is a no-op for every golden.
                cpu.cur_el = 1;
            }
            let m = core::cmp::min(self.slice, target - self.n);
            let m = core::cmp::min(m, self.budget - self.n);
            let mut done = 0u64;
            let mut prev = cpu.pc;
            while done < m {
                // M118b: on a JUMP, try a compiled block for the target.
                // Backward-branch targets (loops) are compiled on first
                // sight — they are the hottest. Forward jumps are left to
                // the interpreter (they run once). Everything is a no-op
                // with PI3_JIT unset.
                #[cfg(not(target_arch = "wasm32"))]
                if self.jit_on && cpu.pc.wrapping_sub(prev) != 4 {
                    let target = cpu.pc;
                    if target <= prev {
                        // backward branch (a loop entry): JIT-compile it.
                        self.jit_run(bus, cpu, target);
                        self.steps += 1;
                        prev = cpu.pc;
                        if self.jit_boundary {
                            self.jit_boundary = false;
                        } else {
                            continue;
                        }
                    }
                }
                let r = cpu.step(bus);
                self.steps += 1;
                if cpu.pc.wrapping_sub(prev) != 4 {
                    self.jumps += 1;
                }
                prev = cpu.pc;
                if let Err(f) = r {
                    // M93c DELIVERED-ABORT CONTINUATION: step() delivers
                    // sync data aborts to the guest vector itself and
                    // reports DataAbort(VA) — the guest handler (not the
                    // harness) owns the fault now. Count the step, keep
                    // running: the vector + handler execute as ordinary
                    // insns on subsequent steps. M100: El0Trap (EL0t
                    // trampoline catch-all) continues the same way. All
                    // OTHER faults still stop the run (first-fault
                    // semantics unchanged).
                    if matches!(f, crate::Fault::DataAbort(_) | crate::Fault::El0Trap(_)) {
                        done += 1;
                        continue;
                    }
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
            // M61 mailbox-IRQ drain watch: the chained handler drains
            // MAIL0_RD (clearing mbx_pending and dropping mbox0) while
            // DAIF is clear; the weighing waiter then observes the
            // completed reply inline. A 1->0 transition of the gated
            // mailbox line across a chunk boundary proves a drain by
            // execution (a stuck line never transitions). Gated on
            // `mbox_drain_log` so the hot loop stays untouched otherwise.
            // M61 send-side watch (MBOXTAG only): any NEW last_write word
            // or DAIF edge while the line is live is traced with pc, so
            // the sender/waiter identity is proven, not guessed.
            if bus.mbox_drain_log {
                let m0 = bus.mbox_pending0() != 0;
                if bus.mbox_prev_live && !m0 {
                    bus.mbox_prev_live = false;
                    self.mbox_drains.push(self.n);
                    if std::env::var("MBOXTAG").is_ok() {
                        eprintln!("MBOXDRAIN n={}", self.n);
                    }
                } else if m0 {
                    bus.mbox_prev_live = true;
                }
                if std::env::var("MBOXTAG").is_ok() {
                    if bus.mbx_last_write != self.mbox_last_seen {
                        self.mbox_last_seen = bus.mbx_last_write;
                        eprintln!(
                            "MBOXSEND n={} pc=0x{:x} daif=0x{:x} word=0x{:08x}",
                            self.n, cpu.pc, cpu.daif, bus.mbx_last_write
                        );
                    }
                    if cpu.daif != self.mbox_daif_last {
                        eprintln!(
                            "MBOXDAIF n={} pc=0x{:x} daif=0x{:x}->0x{:x} mbox0={}",
                            self.n,
                            cpu.pc,
                            self.mbox_daif_last,
                            cpu.daif,
                            bus.mbox_pending0()
                        );
                        self.mbox_daif_last = cpu.daif;
                    }
                }
            }
            if bus.irq_ret_pending {
                bus.irq_ret_pending = false;
                // Resume actuates next pre-chunk. saved_pc/saved_daif already
                // hold the entry snapshot.
                self.resume_armed = true;
            }
            // M65 IRQ-DELIVERY DIAGNOSIS (execution-proven 2026-09-19):
            // the runner delivers into the vector ONLY at chunk edges
            // when the guest is unmasked there. The mailbox waiter runs
            // its weigh loop with DAIF masked (stall daif=0x3), so a
            // level line that is live the whole time is seen by the
            // guest ONLY as +0x60 bit8 inside handlers entered for OTHER
            // sources (timer). The chained handler then serves the
            // timer half and returns without ever reading the IC
            // (ICRD=0 of any offset/size over 11358 chances) — the GPU
            // half starves not because bit8 is missing (value-trace
            // proves val=0x102) but because the dispatch never walks
            // it. MBOXDAIF interleave proves each unmask IS a delivery
            // (3->0x0 at chunk edge, LOCALRD+timer service, 0->0x3 at
            // eret), 11358 of them while mbox0=2.
            // Fresh decision at the CURRENT (end-of-chunk) pc — the facade's
            // irqElr. Delivery sources mirror the hardware/facade split:
            // the legacy GPU line is reported through the local block's
            // CORE_IRQ_SRC bit 8 (which the chained handler reads), while
            // the per-core arch-timer lines surface as CORE_IRQ_SRC bits
            // 0-3 gated by LOCAL_TIMER_INT_CONTROL0. The combined
            // legacy_line() (GPU/timer/DMA mailbox mixture) plus the raw
            // cntp gt condition over-delivers: with the timer enable bit
            // clear, a live cntp compare would enter the vector with no
            // source bit set (proven: LOCALRD legacy=0/cntp=1 reads while
            // the tick was masked). Gate each source on its own enable:
            // mailbox/timer/DMA/GPIO/UART via legacy_line() (their IC
            // enables are already folded in), cntp ONLY when its local
            // enable (bit 1) is set. No in-flight flag: entry masks DAIF,
            // which blocks re-entry while a handler runs (completion
            // unmasks via magic or eret).
            // M61 BARE-METAL COMPAT: the enable gate applies in linux_mode
            // only (see the +0x60 read arm); bare-metal guests (lirq
            // Phase A, rpi-kernel ticks) never touch +0x40 and keep the
            // legacy raw-compare delivery.
            let cntp_gated = if bus.linux_mode {
                (bus.local_timer_ctl0_pub() & (1 << 1)) != 0 && bus.cntp_line()
            } else {
                bus.cntp_line()
            };
            if self.vector_pending.is_none()
                && !cpu.irq_masked()
                && (bus.legacy_line() || cntp_gated)
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
                // DAIF masked, vector by ORIGIN (M77: EL0-origin IRQs take
                // VBAR+0x480 el0_64_irq, EL1 takes VBAR+0x280 el1h_64_irq —
                // the old code always took +0x280, so a timer IRQ firing in
                // EL0 init entered the EL1h handler whose kernel_entry 1
                // reads current via sp_el0 WITHOUT installing it (sp_el0 is
                // still the user stack) — NULL+352 fault at 0e1dd4, nested
                // sp_el0+6648 fault at b9faec, die_lock pending 0x101 spin.
                // Proven by the zzspel0 trail 6.966B-6.967B: ERET to EL0
                // 0x46c100, IRQ-DELIVER at 0x46c0ec, then both EL1 faults
                // with sp_el0=user stack). The IRQ_RET magic path resumes
                // host-assisted guests to saved_pc; eret resumes the rest
                // natively at ELR.
                cpu.elr_el1 = cpu.pc;
                cpu.spsr_el1 = cpu.pstate();
                self.saved_daif = cpu.daif;
                cpu.daif = 0xf;
                self.saved_pc = Some(cpu.pc);
                self.irqs += 1;
                // M76s delivery-source trace (DMATRACE-gated + dma_trace
                // armed): proves DMA/sdhost IRQs deliver (vs latch but
                // never deliver). Zero-cost otherwise.
                if bus.dma_trace && std::env::var("DMATRACE").is_ok() {
                    let dp1 = bus.dma_pending1();
                    let sp2 = bus.sdh_pending2();
                    if dp1 != 0 || sp2 != 0 {
                        let mask: u32 = (0..16usize).fold(0u32, |a, ch| if bus.dma_int_ch_pub(ch) { a | (1 << ch) } else { a });
                        let emask: u32 = (0..16usize).fold(0u32, |a, ch| if bus.dma_end_ch_pub(ch) { a | (1 << ch) } else { a });
                        let line = bus.legacy_line();
                        eprintln!("DMADLV n={} pc=0x{:x} dma_p1=0x{:x} sdh_p2=0x{:x} intmask=0x{:x} endmask=0x{:x} en1=0x{:x} line={} masked={} el={}",
                            self.n, cpu.pc, dp1, sp2, mask, emask,
                            bus.ic_en1_pub(), line as u8, cpu.irq_masked(), cpu.cur_el);
                    }
                }
                if self.irq_pcs.len() < 8 {
                    self.irq_pcs.push(cpu.pc);
                }
                let vbar = if cpu.vbar_el1 == 0 { 0x100000 } else { cpu.vbar_el1 };
                let vec_off = if cpu.cur_el == 0 { 0x480 } else { 0x280 };
                self.vector_pending = Some(vbar + vec_off);
            }
        }
        self.n - start
    }

    /// M118b: run one compiled block for `target` (a loop entry).
    /// Compiles on first sight; on a bail or a runtime boundary the caller
    /// falls back to the interpreter for that instruction (jit_boundary).
    #[cfg(not(target_arch = "wasm32"))]
    fn jit_run(&mut self, bus: &mut crate::Bus, cpu: &mut crate::Cpu, target: u64) {
        use crate::Bus;
        // Only compile HOT blocks. The kernel boot runs thousands of
        // unique backward-branch blocks ONCE each (init code) — compiling
        // them all dominates the boot (~1ms/block). A block must be hit
        // 32 times (interpreted first) before it's worth the compile.
        let slot = ((target >> 4) & 4095) as usize;
        if self.jit_hits[slot] < 32 {
            self.jit_hits[slot] = self.jit_hits[slot].wrapping_add(1);
            self.jit_boundary = true;
            return;
        }
        let mut words = [0u32; 16];
        let mut n = 0usize;
        for i in 0..16u64 {
            match bus.read(target + i * 4, 4) {
                Ok(v) => words[i as usize] = v as u32,
                Err(_) => break,
            }
            n += 1;
        }
        if self.jit.is_none() {
            self.jit = Some(crate::jit::Jit::new());
        }
        let f = match self.jit.as_mut().unwrap().compile(cpu, target, &words[..n]) {
            Some(f) => f,
            None => {
                self.jit_boundary = true;
                return;
            }
        };
        // Seed the flag array from the CPU's actual NZCV — the compiled
        // block reads them (a zeroed array would make every flag-dependent
        // instruction see n=z=c=v=0, wrong whenever the block inherits a
        // nonzero state).
        let (n, z, c, v) = cpu.flags();
        self.jit_nzcv = [n as u8, z as u8, c as u8, v as u8];
        let next = unsafe {
            f(bus as *mut Bus, cpu.x.as_mut_ptr(), &mut cpu.sp, self.jit_nzcv.as_mut_ptr())
        };
        if next < 0 {
            // A memory fault or boundary: resume in the interpreter so it
            // delivers the fault the same way.
            self.jit_boundary = true;
            return;
        }
        cpu.n = self.jit_nzcv[0] != 0;
        cpu.z = self.jit_nzcv[1] != 0;
        cpu.c = self.jit_nzcv[2] != 0;
        cpu.v = self.jit_nzcv[3] != 0;
        cpu.pc = next as u64;
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
