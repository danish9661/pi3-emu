//! Browser (wasm32) binding for pi-cpu: owns one Cpu+Bus+Runner and
//! exposes the slice/input/console surface the demo page needs. The
//! execution logic lives in `runner` (shared verbatim with the native
//! differential runner), so browser behavior matches cpu-diff by
//! construction.

use wasm_bindgen::prelude::*;

use crate::runner::{Runner, BTN_BIT};
use crate::{load_elf, load_linux, Bus, Cpu};

#[wasm_bindgen]
pub struct PiEmu {
    cpu: Cpu,
    bus: Bus,
    runner: Runner,
}

#[wasm_bindgen]
impl PiEmu {
    #[wasm_bindgen(constructor)]
    pub fn new() -> PiEmu {
        PiEmu {
            cpu: Cpu::new(0),
            bus: Bus::new(),
            runner: Runner::new(),
        }
    }

    /// Load a guest ELF into RAM. Returns the entry point. Resets CPU,
    /// devices, and run state (like a fresh boot).
    pub fn load_elf(&mut self, bytes: &[u8]) -> Result<u32, JsValue> {
        // Preserve host tunables across loads (the UI sets these once).
        let vt = self.bus.vt_ips;
        self.bus = Bus::new();
        self.bus.vt_ips = vt;
        let entry = load_elf(&mut self.bus, bytes).map_err(|e| JsValue::from_str(&e))?;
        self.cpu = Cpu::new(entry);
        self.runner = Runner::new();
        self.runner.slice = 4096;
        Ok(entry as u32)
    }

    /// M58 pi-linux track: load the REAL kernel8.img + DTB + initrd
    /// (raw blobs, NOT ELF) at the fixed M56 PAs (kernel 0x200000, DTB
    /// 0x3000000, initrd 0x4000000; RAM expands to 512M) and reset per
    /// the ARM64 boot protocol (x0=DTB PA, EL2, MMU off). The .data
    /// slicing (dtb 0:32753, kernel 32753:22505969, rest initrd — see
    /// public/linux/load.js) happens in JS; the three slices arrive
    /// here as byte arrays. Returns the kernel entry PA.
    pub fn load_linux(
        &mut self,
        kernel: &[u8],
        dtb: &[u8],
        initrd: &[u8],
    ) -> Result<u32, JsValue> {
        // Preserve host tunables across loads (the UI sets these once).
        let vt = self.bus.vt_ips;
        let entry = load_linux(&mut self.bus, kernel, dtb, initrd)
            .map_err(|e| JsValue::from_str(&e))?;
        self.bus.vt_ips = vt;
        self.cpu = Cpu::new(entry);
        self.cpu
            .linux_reset(entry, crate::LINUX_DTB_PA, 0x1FFF_FFF0);
        self.runner = Runner::new();
        self.runner.slice = 4096;
        Ok(entry as u32)
    }

    /// Instructions per sync chunk (default 4096; the pwm guest needs
    /// 512 — its 256-deep FIFO overruns inside 4096-insn chunks).
    pub fn set_slice(&mut self, n: u32) {
        self.runner.slice = n.max(1) as u64;
    }

    /// Virtual-time rate (instructions per second) for the system timer,
    /// 0 = wall-clock legacy behavior. Mirrors `?vt=1` (262144).
    pub fn set_vt_ips(&mut self, ips: u32) {
        self.bus.vt_ips = ips as u64;
    }

    /// Advance the wall clock by `us` microseconds (the UI calls this
    /// per slice from performance.now()). Applies only in wall-clock
    /// mode (vt_ips == 0); virtual-time mode advances per instruction
    /// instead. Drives CLO/CHI reads and the arch-timer counter.
    pub fn wall_tick(&mut self, us: u32) {
        self.bus.wall_tick(us as u64);
    }

    /// Run up to `n` more instructions. Returns instructions executed
    /// (stops early on fault; see `fault()`).
    pub fn run(&mut self, n: u32) -> u32 {
        let target = self.runner.n.saturating_add(n as u64);
        self.runner.run_to(&mut self.cpu, &mut self.bus, target) as u32
    }

    /// Drain console (PL011/mini-UART TX) bytes emitted since last call.
    pub fn take_console(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.bus.console)
    }

    /// Push a key byte into the PL011 RX FIFO (edge-triggered).
    pub fn push_key(&mut self, b: u8) {
        self.bus.uart0_push(b);
    }

    /// Set the GPIO button (pin 29) level.
    pub fn set_button(&mut self, on: bool) {
        if on {
            self.bus.gpio_in |= BTN_BIT;
        } else {
            self.bus.gpio_in &= !BTN_BIT;
        }
    }

    /// GPLEV0 pin levels (LED panel + button reads).
    pub fn gpio_lev(&self) -> u32 {
        self.bus.gpio_lev0()
    }

    /// Flat SD-card image for Save (sector 0 first).
    pub fn export_card(&self) -> Vec<u8> {
        self.bus.sd_export()
    }

    /// Replace the disk image (Load). Must be applied before boot
    /// (like the IndexedDB inject path).
    pub fn import_card(&mut self, bytes: &[u8]) -> bool {
        self.bus.sd_import(bytes)
    }

    /// First fault as a short string, or undefined when clean.
    pub fn fault(&self) -> Option<String> {
        self.runner.fault_string()
    }

    pub fn pc(&self) -> u32 {
        self.cpu.pc as u32
    }

    pub fn sp(&self) -> u32 {
        self.cpu.sp as u32
    }

    /// Executed instruction count (exact to 2^53).
    pub fn insns(&self) -> f64 {
        self.runner.n as f64
    }

    /// X0-X30 as unsigned decimals (status/debug only).
    pub fn regs(&self) -> Vec<String> {
        self.cpu.x.iter().map(|r| r.to_string()).collect()
    }

    /// UI-safe RAM peek (unmapped decodes as zeros; never disturbs
    /// device state).
    pub fn mem_read(&self, addr: u32, len: usize) -> Vec<u8> {
        self.bus.mem_read_bytes(addr as u64, len.min(4096))
    }

    /// Drained PWM audio samples (signed 16-bit), up to `max`.
    pub fn pwm_take(&mut self, max: usize) -> Vec<i16> {
        self.bus.pwm_take(max.min(65536))
    }

    /// Explicit-done park flag per guest (see Bus::done_flag):
    /// 0 = clock/gpio, 1 = mmu, 2 = dma, 3 = pwm, 4 = i2c, 5 = spi, 6 = sd.
    pub fn done(&self, sel: u32) -> u32 {
        self.bus.done_flag(sel)
    }

    /// Framebuffer geometry [w, h, pitch, ready] for the canvas.
    /// Pixels live in guest RAM at 0x200000 (see mem_read).
    pub fn fb_info(&self) -> Vec<u32> {
        let (w, h, p, ready) = self.bus.fb_geometry();
        vec![w, h, p, ready as u32]
    }
}

/// Quad-core SMP session (mirrors SmpRunner): partitioned cores with a
/// shared mailbox, round-robin slices. Owns the whole session so the UI
/// treats it like one emulator.
#[wasm_bindgen]
pub struct PiSmp {
    inner: crate::runner::SmpRunner,
}

#[wasm_bindgen]
impl PiSmp {
    /// Load a guest ELF into all four cores.
    #[wasm_bindgen(constructor)]
    pub fn new(bytes: &[u8]) -> Result<PiSmp, JsValue> {
        let inner =
            crate::runner::SmpRunner::new(bytes).map_err(|e| JsValue::from_str(&e))?;
        Ok(PiSmp { inner })
    }

    /// Run round-robin slices until all cores park, `max_rounds`
    /// rounds, or `budget` total insns. Returns rounds executed.
    pub fn run(&mut self, slice: u32, max_rounds: u32, budget: f64) -> u32 {
        let r0 = self.inner.rounds;
        self.inner.run(slice as u64, max_rounds as u64, budget as u64);
        (self.inner.rounds - r0) as u32
    }

    /// Drain console bytes emitted since last call (round-major order).
    pub fn take_console(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.inner.console)
    }

    /// First fault as a short string, or undefined when clean.
    pub fn fault(&self) -> Option<String> {
        self.inner.fault_string()
    }

    /// [park, counter, msg0..3] state.
    pub fn state(&self) -> Vec<u32> {
        vec![
            self.inner.shared.park,
            self.inner.shared.counter,
            self.inner.shared.msg[0],
            self.inner.shared.msg[1],
            self.inner.shared.msg[2],
            self.inner.shared.msg[3],
        ]
    }

    /// Total insns across cores (exact to 2^53).
    pub fn insns(&self) -> f64 {
        self.inner.total as f64
    }
}
