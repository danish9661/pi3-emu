//! M118: Cranelift JIT for hot straight-line integer blocks (native only).
//!
//! A straight-line block of the top integer arm64 arms is compiled to
//! native code once and called thereafter — no fetch TLB probe, no decode,
//! no match dispatch per instruction. Memory accesses call back into
//! `Bus::read`/`Bus::write` (translation + MMIO dispatch — the same cost
//! the interpreter pays), so the win is on fetch/decode/dispatch, not on
//! memory. Compiled-block ABI:
//!
//!     extern "C" fn(bus: *mut Bus, x: *mut u64, sp: *mut u64,
//!                   nzcv: *mut u8) -> i64
//!
//! return value: the next pc, or a negative status
//! (-1 = unmapped-data fault, -2 = boundary/unsupported — the caller must
//! resume in the interpreter at the returned-far pc stored in `sp[1]`).
//!
//! Arms covered (the top of the fib/kernel hot loops): movz/movk/movn,
//! mov/mvn, add/sub/adds/subs (imm + reg, shifted), and/orr/eor/bic (imm +
//! reg), lsl/lsr/asr (imm + reg), cmp/cmn, cbz/cbnz, tbz/tbnz, b.cond,
//! b/bl/ret/br/blr, adrp/adr, ldr/str/ldrb/strb/ldrh/strh (unsigned
//! offset + unscaled), ldp/stp (offset), nop, csel/cset/csinc/csneg, mul/
//! madd/msub/umulh/smulh/udiv/sdiv. Anything else → boundary.

#![cfg(not(target_arch = "wasm32"))]

use crate::{Bus, Cpu};
use cranelift_codegen::ir::types;
use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlags};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{FuncId, Linkage, Module};
use std::collections::HashMap;

pub type JitFn = unsafe extern "C" fn(*mut Bus, *mut u64, *mut u64, *mut u8) -> i64;

/// Host callbacks the compiled code calls for memory (translation +
/// MMIO dispatch lives in the Bus, shared with the interpreter).
extern "C" fn jit_rd(bus: *mut Bus, va: u64, size: u64, out: *mut u64) -> i32 {
    let bus = unsafe { &mut *bus };
    match bus.read(va, size) {
        Ok(v) => {
            unsafe { *out = v };
            0
        }
        Err(_) => -1,
    }
}
extern "C" fn jit_wr(bus: *mut Bus, va: u64, size: u64, val: u64) -> i32 {
    let bus = unsafe { &mut *bus };
    let r = bus.write(va, size, val);
    if crate::flag("PI3_JITTRACE") {
        eprintln!("jit_wr va=0x{:x} size={} -> {:?}", va, size, r);
    }
    match r {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

pub struct Jit {
    module: JITModule,
    ctx: cranelift_codegen::Context,
    fbc: FunctionBuilderContext,
    rd_id: FuncId,
    wr_id: FuncId,
    pub cache: HashMap<u64, JitFn>,
    /// Blocks that failed to compile — never retried (the bail spam
    /// would otherwise re-attempt them on every backward branch).
    pub failed: std::collections::HashSet<u64>,
    pub compiled: u64,
    pub bails: u64,
}

impl Jit {
    pub fn new() -> Self {
        let mut fb = settings::builder();
        fb.set("opt_level", "speed").unwrap();
        let isa = cranelift_native::builder()
            .expect("native builder")
            .finish(settings::Flags::new(fb))
            .expect("finish isa");
        let mut builder = JITBuilder::with_isa(isa, cranelift_module::default_libcall_names());
        builder.symbols([
            ("jit_rd", jit_rd as *const u8),
            ("jit_wr", jit_wr as *const u8),
        ]);
        let mut module = JITModule::new(builder);
        // Declare both host callbacks.
        let mut rd_sig = module.make_signature();
        for t in [types::I64, types::I64, types::I64, types::I64] {
            rd_sig.params.push(AbiParam::new(t));
        }
        rd_sig.returns.push(AbiParam::new(types::I32));
        let rd_id = module
            .declare_function("jit_rd", Linkage::Import, &rd_sig)
            .expect("decl rd");
        let mut wr_sig = module.make_signature();
        // jit_wr(bus, va, size, val) -> i32: FOUR params, like jit_rd.
        for t in [types::I64, types::I64, types::I64, types::I64] {
            wr_sig.params.push(AbiParam::new(t));
        }
        wr_sig.returns.push(AbiParam::new(types::I32));
        let wr_id = module
            .declare_function("jit_wr", Linkage::Import, &wr_sig)
            .expect("decl wr");
        Jit {
            ctx: module.make_context(),
            module,
            fbc: FunctionBuilderContext::new(),
            rd_id,
            wr_id,
            cache: HashMap::new(),
            failed: std::collections::HashSet::new(),
            compiled: 0,
            bails: 0,
        }
    }

    /// Compile the straight-line block at `pc` (its words were fetched by
    /// the caller). Returns the native fn on success.
    pub fn compile(&mut self, cpu: &mut Cpu, pc: u64, words: &[u32]) -> Option<JitFn> {
        let key = pc;
        if let Some(f) = self.cache.get(&key) {
            return Some(*f);
        }
        if self.failed.contains(&key) {
            return None;
        }
        let f = self.compile_inner(cpu, pc, words);
        if let Some(f) = f {
            self.cache.insert(key, f);
            self.compiled += 1;
        } else {
            self.bails += 1;
            self.failed.insert(key);
        }
        f
    }

    fn compile_inner(&mut self, _cpu: &mut Cpu, pc: u64, words: &[u32]) -> Option<JitFn> {
        if crate::flag("PI3_JITTRACE") { eprintln!("jit: compile_inner pc=0x{:x} words={}", pc, words.len()); }
        let _dump = pc == 0x100024;
        // fn(bus, x, sp, nzcv) -> i64
        let mut sig = self.module.make_signature();
        for _ in 0..4 {
            sig.params.push(AbiParam::new(types::I64));
        }
        sig.returns.push(AbiParam::new(types::I64));
        let name = format!("b_{:x}", pc);
        let fid = self
            .module
            .declare_function(&name, Linkage::Local, &sig)
            .map_err(|e| { eprintln!("jit: declare: {}", e); e })
            .ok()?;
        // A FRESH Context AND FunctionBuilderContext per block: reusing
        // either contaminates the block (the tail block returns the
        // loop-body's garbage; reused fbc accumulates sigs/import-refs
        // until Cranelift's own passes panic "entry block unknown").
        self.ctx = self.module.make_context();
        self.fbc = FunctionBuilderContext::new();
        self.ctx.func.signature = sig;

        let mut fb = FunctionBuilder::new(&mut self.ctx.func, &mut self.fbc);
        let entry = fb.create_block();
        fb.switch_to_block(entry);
        fb.append_block_params_for_function_params(entry);
        let busp = fb.block_params(entry)[0];
        let xp = fb.block_params(entry)[1];
        let spp = fb.block_params(entry)[2];
        let nzvp = fb.block_params(entry)[3];

        let fault_exit = fb.create_block();
        let mut emitter = Emitter {
            fb: &mut fb,
            busp,
            xp,
            spp,
            nzvp,
            module: &mut self.module,
            rd_id: self.rd_id,
            wr_id: self.wr_id,
            fault_exit,
            cont: Vec::new(),
        };

        let mut cur = pc;
        let mut terminated = false;
        for &w in words {
            cur = cur.wrapping_add(4);
            match emitter.insn(w, cur) {
                None => {
                    if crate::flag("PI3_JITTRACE") { eprintln!("jit: bail at pc=0x{:x} word=0x{:08x}", cur.wrapping_sub(4), w); }
                    return None;
                }
                Some(true) => {
                    terminated = true;
                    break;
                }
                Some(false) => {}
            }
        }
        // Fall off the end: return the next pc (only if no branch ended it).
        // This must come FIRST — switching to fault_exit asserts the entry
        // block is filled, which is only true after this terminator.
        if !terminated {
            let ret = fb.ins().iconst(types::I64, cur as i64);
            fb.ins().return_(&[ret]);
        }
        // Populate the shared fault-exit block (return -1).
        fb.switch_to_block(fault_exit);
        fb.seal_block(fault_exit);
        let neg1 = fb.ins().iconst(types::I64, -1i64);
        fb.ins().return_(&[neg1]);
        fb.seal_all_blocks();
        fb.finalize();
        // Cranelift's own passes can panic on pathological IR (e.g.
        // "remove_constant_phis: entry block unknown" on some blocks). A
        // JIT is a best-effort accelerator, never a crash source: catch
        // the panic and fall back to the interpreter.
        if std::env::var("PI3_JITDUMP").is_ok() {
            eprintln!("IR(pc=0x{:x}):\n{}", pc, self.ctx.func);
        }
        let define_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.module.define_function(fid, &mut self.ctx)
        }));
        let define_result = match define_result {
            Ok(r) => r,
            Err(_) => {
                eprintln!("jit: cranelift panic, bailing (pc=0x{:x})", pc);
                return None;
            }
        };
        match define_result {
            Ok(_) => { if _dump { eprintln!("IR:\n{}", self.ctx.func); } }
            Err(e) => {
                eprintln!("jit: define: {:?}", e);
                eprintln!("{}", self.ctx.func);
                return None;
            }
        }
        self.module.finalize_definitions().map_err(|e| { eprintln!("jit: finalize: {}", e); e }).ok()?;
        let ptr = self.module.get_finalized_function(fid);
        Some(unsafe { core::mem::transmute::<_, JitFn>(ptr) })
    }
}

struct Emitter<'a, 'b> {
    fb: &'b mut FunctionBuilder<'a>,
    busp: cranelift_codegen::ir::Value,
    xp: cranelift_codegen::ir::Value,
    spp: cranelift_codegen::ir::Value,
    nzvp: cranelift_codegen::ir::Value,
    module: &'b mut JITModule,
    rd_id: FuncId,
    wr_id: FuncId,
    fault_exit: cranelift_codegen::ir::Block,
    cont: Vec<cranelift_codegen::ir::Block>,
}

type V = cranelift_codegen::ir::Value;

impl<'a, 'b> Emitter<'a, 'b> {
    /// Sign-extend an N-bit immediate (branch offsets go both ways).
    fn sext(v: u64, bits: u32) -> i64 {
        let sh = 64 - bits;
        ((v << sh) as i64) >> sh
    }

    fn iconst(&mut self, v: u64) -> V {
        self.fb.ins().iconst(types::I64, v as i64)
    }
    fn load_x(&mut self, i: u32) -> V {
        if i == 31 {
            // XZR: the register file has 31 entries (x0..x30) + a
            // separate SP, so reading index 31 must yield 0, not memory.
            self.iconst(0)
        } else {
            self.fb
                .ins()
                .load(types::I64, MemFlags::trusted(), self.xp, (i * 8) as i32)
        }
    }
    fn store_x(&mut self, i: u32, v: V) {
        if i != 31 {
            self.fb
                .ins()
                .store(MemFlags::trusted(), v, self.xp, (i * 8) as i32);
        }
    }
    fn load_sp(&mut self) -> V {
        self.fb
            .ins()
            .load(types::I64, MemFlags::trusted(), self.spp, 0)
    }
    fn store_sp(&mut self, v: V) {
        self.fb
            .ins()
            .store(MemFlags::trusted(), v, self.spp, 0);
    }
    fn w32(&mut self, v: V) -> V {
        self.fb.ins().band_imm(v, 0xffff_ffff)
    }

    /// One instruction. `cur` is the pc of the NEXT instruction.
    fn insn(&mut self, w: u32, cur: u64) -> Option<bool> {
        let sf = (w >> 31) & 1;
        let rd = w & 31;
        let rn = (w >> 5) & 31;
        let rm = (w >> 16) & 31;
        // --- branches (block terminators are handled by the caller; a
        // mid-block branch is a boundary) ---
        if (w & 0x7c000000) == 0x14000000 {
            // b / bl — end of block; the compiled code returns the target.
            let off = Self::sext(((w & 0x03ff_ffff) as u64) << 2, 28);
            let target = (cur.wrapping_sub(4) as i64).wrapping_add(off) as u64;
            if (w >> 31) & 1 == 1 {
                // bl: lr = cur
                let lr = self.iconst(cur);
                self.store_x(30, lr);
            }
            let t = self.iconst(target);
            self.fb.ins().return_(&[t]);
            // A b/bl TERMINATES the block — returning false here let the
            // fall-through append a second return to a filled block.
            return Some(true);
        }
        if (w & 0xffff_fc1f) == 0xd65f_0000 {
            // ret
            let lr = self.load_x(30);
            self.fb.ins().return_(&[lr]);
            return Some(true);
        }
        if (w & 0xffe0_fc00) == 0xd61f_0000 {
            // br/blr
            let t = self.load_x(rn);
            self.fb.ins().return_(&[t]);
            return Some(true);
        }
        // cbz/cbnz
        if (w & 0x7e00_0000) == 0x3400_0000 {
            let off = Self::sext((((w >> 5) & 0x7ffff) as u64) << 2, 21);
            let target = (cur.wrapping_sub(4) as i64).wrapping_add(off) as u64;
            let v = self.load_x(rd);
            let v = if sf == 0 { self.w32(v) } else { v };
            let cond = if (w >> 24) & 1 == 0 {
                // cbz
                { let _c = self.fb.ins().icmp_imm(cranelift_codegen::ir::condcodes::IntCC::Equal, v, 0); self.bool64(_c) }
            } else {
                { let _c = self.fb.ins().icmp_imm(cranelift_codegen::ir::condcodes::IntCC::NotEqual, v, 0); self.bool64(_c) }
            };
            let t = self.iconst(target);
            let fall = self.iconst(cur);
            // Cranelift `select` wants an I8 boolean; derive it from the
            // I64 condition with a compare-to-zero.
            let c8 = self
                .fb
                .ins()
                .icmp_imm(cranelift_codegen::ir::condcodes::IntCC::NotEqual, cond, 0);
            let sel = self.fb.ins().select(c8, t, fall);
            self.fb.ins().return_(&[sel]);
            return Some(true);
        }
        // tbz/tbnz
        if (w & 0x7e00_0000) == 0x3600_0000 {
            let bit = (((w >> 26) & 1) << 5) | ((w >> 19) & 31);
            let off = Self::sext((((w >> 5) & 0x3fff) as u64) << 2, 16);
            let target = (cur.wrapping_sub(4) as i64).wrapping_add(off) as u64;
            let v = self.load_x(rd);
            let bitv = self.fb.ins().band_imm(v, 1i64 << bit);
            let cond = if (w >> 24) & 1 == 0 {
                // tbz
                { let _c = self.fb.ins().icmp_imm(cranelift_codegen::ir::condcodes::IntCC::Equal, bitv, 0); self.bool64(_c) }
            } else {
                { let _c = self.fb.ins().icmp_imm(cranelift_codegen::ir::condcodes::IntCC::NotEqual, bitv, 0); self.bool64(_c) }
            };
            let t = self.iconst(target);
            let fall = self.iconst(cur);
            // Cranelift `select` wants an I8 boolean; derive it from the
            // I64 condition with a compare-to-zero.
            let c8 = self
                .fb
                .ins()
                .icmp_imm(cranelift_codegen::ir::condcodes::IntCC::NotEqual, cond, 0);
            let sel = self.fb.ins().select(c8, t, fall);
            self.fb.ins().return_(&[sel]);
            return Some(true);
        }
        // b.cond
        if (w & 0xff00_0010) == 0x5400_0000 {
            let off = Self::sext((((w >> 5) & 0x7ffff) as u64) << 2, 21);
            let target = (cur.wrapping_sub(4) as i64).wrapping_add(off) as u64;
            let cond = self.cond(w & 15)?;
            let t = self.iconst(target);
            let fall = self.iconst(cur);
            // Cranelift `select` wants an I8 boolean; derive it from the
            // I64 condition with a compare-to-zero.
            let c8 = self
                .fb
                .ins()
                .icmp_imm(cranelift_codegen::ir::condcodes::IntCC::NotEqual, cond, 0);
            let sel = self.fb.ins().select(c8, t, fall);
            self.fb.ins().return_(&[sel]);
            return Some(true);
        }
        // adrp / adr
        if (w & 0x9f00_0000) == 0x9000_0000 || (w & 0x9f00_0000) == 0x1000_0000 {
            let immhi = ((w >> 5) & 0x7ffff) as u64;
            let immlo = ((w >> 29) & 3) as u64;
            let imm = (((immhi << 2) | immlo) as i64) << if (w >> 31) & 1 == 1 { 12 } else { 0 };
            let page = (cur.wrapping_sub(4)) & !0xfffu64;
            let base = if (w >> 31) & 1 == 1 { page } else { cur.wrapping_sub(4) };
            let v = (base as i64).wrapping_add(imm);
            let _tmp = self.iconst(v as u64);
                self.store_x(rd, _tmp);
            return Some(false);
        }
        // movz/movk/movn
        if (w & 0x7f80_0000) == 0x5280_0000 || (w & 0x7f80_0000) == 0x7280_0000 || (w & 0x7f80_0000) == 0x1280_0000 {
            let opc = (w >> 29) & 3;
            let hw = (w >> 21) & 3;
            let imm = ((w >> 5) & 0xffff) as u64;
            let shift = hw * 16;
            if opc == 0 {
                // movn
                let mut v = !(imm << shift);
                if sf == 0 {
                    v &= 0xffff_ffff;
                }
                let _tmp = self.iconst(v);
                self.store_x(rd, _tmp);
            } else if opc == 2 {
                // movz
                let v = imm << shift;
                let _tmp = self.iconst(v);
                self.store_x(rd, _tmp);
            } else {
                // movk
                let cur_v = self.load_x(rd);
                let mask = !(0xffffu64 << shift);
                let masked = self.fb.ins().band_imm(cur_v, mask as i64);
                let v = self.fb.ins().bor_imm(masked, (imm << shift) as i64);
                let v = if sf == 0 { self.w32(v) } else { v };
                self.store_x(rd, v);
            }
            return Some(false);
        }
        // add/sub (immediate)
        if (w & 0x1f00_0000) == 0x1100_0000 {
            let op = (w >> 30) & 1;
            let s = (w >> 29) & 1;
            let imm = ((w >> 10) & 0xfff) as u64;
            let imm = if (w >> 22) & 1 == 1 { imm << 12 } else { imm };
            let a = if rn == 31 { self.load_sp() } else { self.load_x(rn) };
            let b = self.iconst(imm);
            let v = if op == 0 { self.fb.ins().iadd(a, b) } else { self.fb.ins().isub(a, b) };
            let v = if sf == 0 { self.w32(v) } else { v };
            if s == 1 {
                self.set_flags(a, b, v, op, sf);
            }
            if rd == 31 && s == 0 {
                self.store_sp(v);
            } else if rd != 31 {
                self.store_x(rd, v);
            }
            return Some(false);
        }
        // ADD/SUB (shifted OR extended register). The interpreter's
        // disambiguation (lib.rs, fuzzer-pinned): bit 21 selects EXTENDED
        // (uxtw/uxtx/sxtb/sxth/sxtw/sxtx with option + imm3) vs SHIFTED
        // (lsl/lsr/asr with imm6). The old code read option+imm3 as imm6,
        // which is the extend-group DIFF set.
        if (w & 0x1f00_0000) == 0x0b00_0000 {
            let op = (w >> 30) & 1;
            let s = (w >> 29) & 1;
            let extended = (w >> 21) & 1 == 1;
            let mut b = self.load_x(rm);
            if extended {
                b = self.extend_reg(b, (w >> 13) & 7, ((w >> 10) & 7) as u32);
            } else {
                if sf == 0 {
                    b = self.w32(b);
                }
                let imm6 = ((w >> 10) & 0x3f) as u32;
                if imm6 != 0 {
                    b = self.shift((w >> 22) & 3, b, imm6, sf);
                }
            }
            // Rn==31: SP everywhere EXCEPT shifted-form SUB (S=0 or S=1),
            // where it reads XZR (lib.rs's proven rule; `neg x1,x1`).
            let a = if rn == 31 && !extended && op == 1 {
                self.load_x(31) // XZR
            } else if rn == 31 {
                self.load_sp()
            } else {
                self.load_x(rn)
            };
            let v = if op == 0 { self.fb.ins().iadd(a, b) } else { self.fb.ins().isub(a, b) };
            let v = if sf == 0 { self.w32(v) } else { v };
            if s == 1 {
                self.set_flags(a, b, v, op, sf);
            }
            if rd == 31 && s == 0 {
                self.store_sp(v);
            } else if rd != 31 {
                self.store_x(rd, v);
            }
            return Some(false);
        }
        // logical (shifted register)
        if (w & 0x1f00_0000) == 0x0a00_0000 {
            let opc = (w >> 29) & 3;
            let n = (w >> 21) & 1;
            let sh = (w >> 22) & 3;
            let imm6 = ((w >> 10) & 0x3f) as u32;
            let a = self.load_x(rn);
            let mut b = self.load_x(rm);
            if sf == 0 {
                b = self.w32(b);
            }
            if imm6 != 0 {
                b = self.shift(sh, b, imm6, sf);
            }
            // The N bit inverts the SECOND operand (BIC/ORN/EON), for
            // every opcode — the old code applied it only to AND.
            if n == 1 {
                b = self.fb.ins().bnot(b);
            }
            let v = match opc {
                0 => self.fb.ins().band(a, b),
                1 => self.fb.ins().bor(a, b),
                2 => self.fb.ins().bxor(a, b),
                _ => self.fb.ins().band(a, b), // ands -> sets flags below
            };
            let v = if sf == 0 { self.w32(v) } else { v };
            if opc == 3 {
                // ands: N/Z from the result; C/V = 0 (shift_carry = 0 for
                // the non-rotate forms — rotate is rare and currently a
                // documented gap).
                self.set_nz(v, sf);
                let zero = self.iconst(0);
                self.store_flag(2, zero);
                self.store_flag(3, zero);
            }
            self.store_x(rd, v);
            return Some(false);
        }
        // lsl/lsr/asr (immediate) — UBFM/SBFM
        if (w & 0x1f80_0000) == 0x5380_0000 || (w & 0x1f80_0000) == 0x5300_0000 || (w & 0x1f80_0000) == 0x1300_0000 {
            let opc = (w >> 29) & 3;
            let n = (w >> 22) & 1;
            let immr = ((w >> 16) & 0x3f) as u32;
            let imms = ((w >> 10) & 0x3f) as u32;
            let a = self.load_x(rn);
            let v = self.ubfm(a, opc, n, immr, imms, sf)?;
            self.store_x(rd, v);
            return Some(false);
        }
        // cmp/cmn (immediate, add/sub with S and rd=31)
        if (w & 0x1f00_001f) == 0x1100_001f || (w & 0x1f00_001f) == 0x1300_001f {
            let op = (w >> 30) & 1;
            let imm = ((w >> 10) & 0xfff) as u64;
            let imm = if (w >> 22) & 1 == 1 { imm << 12 } else { imm };
            let a = if rn == 31 { self.load_sp() } else { self.load_x(rn) };
            let b = self.iconst(imm);
            let v = if op == 0 { self.fb.ins().iadd(a, b) } else { self.fb.ins().isub(a, b) };
            let b_eff = if op == 0 { b } else { self.fb.ins().bnot(b) };
                let vr = if sf == 0 { self.w32(v) } else { v };
                self.set_flags(a, b_eff, vr, op, sf);
            return Some(false);
        }
        // mul/madd/msub
        if (w & 0x7fe0_0000) == 0x1b00_0000 || (w & 0x7fe0_0000) == 0x9b00_0000 {
            let ra = (w >> 10) & 31;
            // MADD vs MSUB is bit 15 (assembler-truth, sel.s).
            let o1 = (w >> 15) & 1;
            let a = self.load_x(rm);
            let b = self.load_x(rn);
            let acc = self.load_x(ra);
            let p = self.fb.ins().imul(a, b);
            let v = if o1 == 0 {
                self.fb.ins().iadd(acc, p)
            } else {
                self.fb.ins().isub(acc, p)
            };
            let v = if sf == 0 { self.w32(v) } else { v };
            self.store_x(rd, v);
            return Some(false);
        }
        // umulh/smulh: the 2-source high-multiply class (bits[31:21]).
        // UMULH = 0x1bc00000, SMULH = 0x1b400000 (bit 22 picks signed).
        if (w & 0x7f60_0000) == 0x1b40_0000 {
            // UMULH vs SMULH is bit 22 (assembler-truth, sel.s).
            let o1 = (w >> 22) & 1;
            let a = self.load_x(rm);
            let b = self.load_x(rn);
            let v = if o1 == 0 {
                self.fb.ins().umulhi(a, b)
            } else {
                self.fb.ins().smulhi(a, b)
            };
            self.store_x(rd, v);
            return Some(false);
        }
        // udiv/sdiv
        // UDIV/SDIV: 2-source class 0x1ac00000; opcode bits[15:10] = 1
        // selects SDIV (assembler-truth, sel.s).
        // 2-source class 0x1ac00000: opcode bits[15:10] selects the op
        // (assembler-truth, src2.s): 2 UDIV, 3 SDIV, 8 LSLV, 9 LSRV,
        // 10 ASRV, 11 RORV. The old code treated every op as a div and
        // crashed on the shift forms.
        if (w & 0x7fe0_d000) == 0x1ac0_0000 {
            let op = (w >> 10) & 0x3f;
            let a = self.load_x(rn);
            let b = self.load_x(rm);
            let v = match op {
                2 | 3 => {
                    let zero = self.iconst(0);
                    let q = if op == 2 {
                        self.fb.ins().udiv(a, b)
                    } else {
                        self.fb.ins().sdiv(a, b)
                    };
                    // div-by-zero -> 0 (ARM semantics)
                    let bnz0 = self
                        .fb
                        .ins()
                        .icmp_imm(cranelift_codegen::ir::condcodes::IntCC::NotEqual, b, 0);
                    let bnz = self.bool64(bnz0);
                    self.fb.ins().select(bnz, q, zero)
                }
                8 => self.fb.ins().ishl(a, b),
                9 => self.fb.ins().ushr(a, b),
                10 => self.fb.ins().sshr(a, b),
                11 => {
                    // ror = (a >> b) | (a << (width - b)); b==0 -> a.
                    let l = self.fb.ins().ushr(a, b);
                    let width = if sf == 0 { 32i64 } else { 64i64 };
                    let comp = self.fb.ins().irsub_imm(b, width);
                    let h = self.fb.ins().ishl(a, comp);
                    self.fb.ins().bor(l, h)
                }
                _ => return None,
            };
            let v = if sf == 0 { self.w32(v) } else { v };
            self.store_x(rd, v);
            return Some(false);
        }
        // LDRSB/LDRSH/LDRSW (signed loads, unsigned offset): the 0x39
        // class with opc(bits[23:22]) == 2 or 3. size(bits[31:30]) picks
        // the element: 0 = byte, 1 = halfword, 2 = word; opc 2 = sign-extend
        // to 64-bit, opc 3 = sign-extend to 32-bit (LDRSW is size 2 / opc 2).
        if (w & 0x3b00_0000) == 0x3900_0000 && (w >> 26) & 1 == 0 && ((w >> 22) & 3) >= 2 {
            let opc = (w >> 22) & 3;
            let size = (w >> 30) & 3;
            let imm12 = ((w >> 10) & 0xfff) as u64;
            let off = imm12 << size;
            let base = if rn == 31 { self.load_sp() } else { self.load_x(rn) };
            let addr = self.fb.ins().iadd_imm(base, off as i64);
            let bytes = 1u64 << size;
            let v = self.mem_rd(addr, bytes)?;
            let narrow = match size {
                0 => self.fb.ins().ireduce(types::I8, v),
                1 => self.fb.ins().ireduce(types::I16, v),
                _ => self.fb.ins().ireduce(types::I32, v),
            };
            let out = self.fb.ins().sextend(types::I64, narrow);
            // LDRSB/SH/SW to W (opc 3) zero the top 32 (X-result masked).
            let out = if opc == 3 { self.w32(out) } else { out };
            self.store_x(rd, out);
            return Some(false);
        }
        // ldr/str (unsigned offset) — INTEGER only (bit 26 = V=0); the
        // SIMD&FP forms (V=1, e.g. `ldr d1, [x8]`) are a different class
        // the JIT bails to the interpreter for.
        if (w & 0x3b00_0000) == 0x3900_0000 && (w >> 26) & 1 == 0 {
            let is_load = (w >> 22) & 1;
            let size = (w >> 30) & 3;
            let imm12 = ((w >> 10) & 0xfff) as u64;
            let off = imm12 << size;
            let base = if rn == 31 { self.load_sp() } else { self.load_x(rn) };
            let addr = self.fb.ins().iadd_imm(base, off as i64);
            let bytes = 1u64 << size;
            if is_load == 1 {
                let v = self.mem_rd(addr, bytes)?;
                self.store_x(rd, v);
            } else {
                let v = if rd == 31 { self.iconst(0) } else { self.load_x(rd) };
                self.mem_wr(addr, bytes, v)?;
            }
            return Some(false);
        }
        // ldur/stur + post/pre-index (signed 9-bit) — INTEGER only. Addressing mode per
        // (bit11, bit10): 00 unscaled (no writeback), 01 post-index, 11
        // pre-index — the interpreter's proven rule (lib.rs). The old code
        // only did unscaled, so `ldr x0,[x1],#8` (post) never wrote x1 back.
        if (w & 0x3b00_0000) == 0x3800_0000 && (w >> 26) & 1 == 0 {
            let is_load = (w >> 22) & 1;
            let size = (w >> 30) & 3;
            if (w >> 21) & 1 == 1 {
                // REGISTER OFFSET (the interpreter's proven rule, lib.rs):
                // option = bits[15:13], S = bit12, amount = if S { size }
                // else 0 (LSL #esz-log2). Then off = extend_reg(rm, option,
                // amount), addr = base + off.
                let option = (w >> 13) & 7;
                let s = (w >> 12) & 1;
                let amount = if s == 1 { size } else { 0 };
                let base = if rn == 31 { self.load_sp() } else { self.load_x(rn) };
                let rmv = self.load_x(rm);
                let off = self.extend_reg(rmv, option, amount as u32);
                let addr = self.fb.ins().iadd(base, off);
                let bytes = 1u64 << size;
                if is_load == 1 {
                    let v = self.mem_rd(addr, bytes)?;
                    self.store_x(rd, v);
                } else {
                    let v = if rd == 31 { self.iconst(0) } else { self.load_x(rd) };
                    self.mem_wr(addr, bytes, v)?;
                }
                return Some(false);
            }
            // Sign-extend the 9-bit immediate with the PROVEN sext helper
            // (same one the branches use — the i32 shift form mis-shifted
            // and left the offset at 0, so jit_wr saw va=base).
            let imm9 = Self::sext(((w >> 12) & 0x1ff) as u64, 9);
            let mode = ((w >> 11) & 1) << 1 | ((w >> 10) & 1);
            let base = if rn == 31 { self.load_sp() } else { self.load_x(rn) };
            // Use iadd(base, iconst(imm9)) so a NEGATIVE offset is applied
            // — iadd_imm with a negative immediate silently dropped the
            // offset (jit_wr saw va=base, never base+off, proven by the
            // stur #-256 trace).
            let offv = self.iconst((imm9 as i64) as u64);
            let addr = self.fb.ins().iadd(base, offv);
            let bytes = 1u64 << size;
            // The access address: UNSCALED and POST use base+offset
            // (unscaled IS base+offset; post accesses base and writes the
            // offset back AFTER); only PRE also uses base+offset. The old
            // code used bare `base` for unscaled/post, so stur/ldur wrote
            // the base with NO offset (jit_wr saw va=base).
            let acc = if mode == 0b01 { base } else { addr };
            if is_load == 1 {
                let v = self.mem_rd(acc, bytes)?;
                self.store_x(rd, v);
            } else {
                let v = if rd == 31 { self.iconst(0) } else { self.load_x(rd) };
                self.mem_wr(acc, bytes, v)?;
            }
            // Writeback: both post and pre land the base at base+offset
            // (= addr); SP gets store_sp.
            if mode != 0b00 {
                if rn == 31 {
                    self.store_sp(addr);
                } else {
                    self.store_x(rn, addr);
                }
            }
            return Some(false);
        }
        // ldp/stp (signed offset + post/pre-index, signed 7-bit scaled).
        // Addressing mode bits[24:23]: 00/10 offset (addr = base+off, no
        // writeback), 11 pre (addr = base+off, wb = addr), 01 post
        // (addr = base, wb = base+off FIRST) — the interpreter's rule.
        if ((w & 0x3a00_0000) == 0x2800_0000 || (w & 0x3a00_0000) == 0x2900_0000 || (w & 0x3a00_0000) == 0x2c00_0000 || (w & 0x3a00_0000) == 0x2d00_0000) && (w >> 26) & 1 == 0 {
            let is_load = (w >> 22) & 1;
            let size_class = (w >> 30) & 3; // 0=32-bit, 2=64-bit
            if size_class == 1 {
                // LDPSW (signed-word pair) is a different instruction —
                // not in the integer subset.
                return None;
            }
            let rt = rd;
            let rt2 = (w >> 10) & 31;
            let scale = if size_class == 0 { 2 } else { 3 };
            let imm7 = ((((w >> 15) & 0x7f) as i32) << 25 >> 25) as i64;
            let off = imm7 << scale;
            let mode = (w >> 23) & 3;
            let base = if rn == 31 { self.load_sp() } else { self.load_x(rn) };
            let baseoff = self.fb.ins().iadd_imm(base, off);
            // Pre (11) accesses at base+off; offset (00/10) at base+off;
            // post (01) at base (writeback is base+off, applied FIRST).
            let addr = if mode == 0b01 { base } else { baseoff };
            if mode == 0b01 || mode == 0b11 {
                let wb = baseoff;
                if rn == 31 {
                    self.store_sp(wb);
                } else {
                    self.store_x(rn, wb);
                }
            }
            let bytes = 1u64 << scale;
            if is_load == 1 {
                let a = self.mem_rd(addr, bytes)?;
                let addr2 = self.fb.ins().iadd_imm(addr, bytes as i64);
                let b = self.mem_rd(addr2, bytes)?;
                if size_class == 2 {
                    self.store_x(rt, a);
                    self.store_x(rt2, b);
                } else {
                    let aw = self.w32(a);
                    self.store_x(rt, aw);
                    let bw = self.w32(b);
                    self.store_x(rt2, bw);
                }
            } else {
                let a = if rt == 31 { self.iconst(0) } else { self.load_x(rt) };
                let b = if rt2 == 31 { self.iconst(0) } else { self.load_x(rt2) };
                let a = if size_class == 2 { a } else { self.w32(a) };
                let b = if size_class == 2 { b } else { self.w32(b) };
                self.mem_wr(addr, bytes, a)?;
                let addr3 = self.fb.ins().iadd_imm(addr, bytes as i64);
                self.mem_wr(addr3, bytes, b)?;
            }
            return Some(false);
        }
        // nop / hints
        if (w >> 12) == 0xd5032 || (w >> 12) == 0xd5033 {
            return Some(false);
        }
        // csel / cset / csinc / csneg
        if (w & 0x7fe0_0000) == 0x1a80_0000 || (w & 0x7fe0_0000) == 0x5a80_0000 {
            let o2 = (w >> 10) & 1;
            let op = (w >> 30) & 1;
            let cond = (w >> 12) & 15;
            let a = self.load_x(rn);
            let b = self.load_x(rm);
            let b = if op == 1 { self.fb.ins().bnot(b) } else { b };
            let b = if o2 == 1 { self.fb.ins().iadd_imm(b, 1) } else { b };
            let c = self.cond(cond)?;
            let v = self.fb.ins().select(c, a, b);
            let v = if sf == 0 { self.w32(v) } else { v };
            self.store_x(rd, v);
            return Some(false);
        }
        // unknown / unsupported: boundary (caller resumes in the interpreter)
        None
    }

    /// extend_reg (mirrors lib.rs): zero/sign-extend the low bits of `v`
    /// per `option` (0 uxtb, 1 uxth, 2 uxtw, 3 uxtx, 4 sxtb, 5 sxth,
    /// 6 sxtw, 7 sxtx) and shift left by `amount`.
    fn extend_reg(&mut self, v: V, option: u32, amount: u32) -> V {
        let masked = match option & 7 {
            0 => self.fb.ins().band_imm(v, 0xff),
            1 => self.fb.ins().band_imm(v, 0xffff),
            2 => self.fb.ins().band_imm(v, 0xffff_ffff),
            4 => {
                let b = self.fb.ins().ireduce(types::I8, v);
                self.fb.ins().sextend(types::I64, b)
            }
            5 => {
                let h = self.fb.ins().ireduce(types::I16, v);
                self.fb.ins().sextend(types::I64, h)
            }
            6 => {
                let w = self.fb.ins().ireduce(types::I32, v);
                self.fb.ins().sextend(types::I64, w)
            }
            _ => v,
        };
        if amount == 0 {
            masked
        } else {
            self.fb.ins().ishl_imm(masked, amount as i64)
        }
    }

    fn shift(&mut self, sh: u32, v: V, amount: u32, sf: u32) -> V {
        // ARM shift types (bits[23:22]): 0 LSL, 1 LSR, 2 ASR, 3 ROR. The
        // old mapping had LSL/LSR swapped.
        let a = amount as i64;
        match sh {
            0 => self.fb.ins().ishl_imm(v, a),      // LSL
            1 => self.fb.ins().ushr_imm(v, a),      // LSR
            2 => self.fb.ins().sshr_imm(v, a),      // ASR
            _ => self.ror_imm(v, a, if sf == 0 { 32 } else { 64 }),
        }
    }

    fn ubfm(&mut self, a: V, opc: u32, _n: u32, immr: u32, imms: u32, sf: u32) -> Option<V> {
        // opc: 0 SBFM, 1 BFM(bfi), 2 UBFM. Correct field semantics (the
        // fuzzer's bitfield group): with R=immr, S=imms, width = 32/64:
        //   S >= R -> extract bits [S:R] (UBFX/SBFX), else insert at
        //   (width - R) (UBFIZ/SBFIZ). The old code shifted by (R - S)
        //   which is only the degenerate LSL alias's shape.
        let w = if sf == 0 { 32 } else { 64 };
        if opc == 1 {
            return None; // BFM rare in hot loops; boundary
        }
        let r = immr as i64;
        let sm = imms as i64;
        if sm >= r {
            // extract [S:R] = (src >> R) & ((1 << (S - R + 1)) - 1)
            let field = self.fb.ins().ushr_imm(a, r);
            let bits = sm - r + 1;
            let mask = if bits >= 64 { -1i64 } else { (1i64 << bits) - 1 };
            let v = self.fb.ins().band_imm(field, mask);
            if opc == 0 {
                // SBFX: sign-extend from bit `bits`.
                if bits < 64 {
                    let l = self.fb.ins().ishl_imm(v, 64 - bits);
                    return Some(self.fb.ins().sshr_imm(l, 64 - bits));
                }
            }
            return Some(v);
        }
        // INSERT (S < R), the ROR form: dst = ROR(src, R) & ROR(tmask, R)
        // where tmask = (1 << (S + 1)) - 1. (The old code masked AFTER the
        // left shift, which keeps the field but also the bits ABOVE it.)
        let width = w;
        let tmask_val = if sm + 1 >= 64 { u64::MAX } else { (1u64 << (sm + 1)) - 1 };
        let tmask = self.iconst(tmask_val);
        let rotated_src = self.ror_imm(a, r, width);
        let rotated_tmask = self.ror_imm(tmask, r, width);
        let v = self.fb.ins().band(rotated_src, rotated_tmask);
        if opc == 0 {
            // SBFIZ: sign-extend from bit `d + 1` where `d = (S - R) mod
            // width` — the interpreter's proven formula (lib.rs SBFM
            // general case). The old code used `S + 1`, which is only right
            // for the extract form and mis-sign-extends inserts.
            let d = ((sm - r).rem_euclid(w)) as i64;
            let bits = d + 1;
            if bits < 64 {
                let l = self.fb.ins().ishl_imm(v, 64 - bits);
                return Some(self.fb.ins().sshr_imm(l, 64 - bits));
            }
        }
        Some(v)
    }

    /// ROR `v` by `amount` over `width` bits: (v >> a) | (v << (w - a)).
    fn ror_imm(&mut self, v: V, amount: i64, width: i64) -> V {
        let a = ((amount % width) + width) % width;
        if a == 0 {
            return v;
        }
        let l = self.fb.ins().ushr_imm(v, a);
        let h = self.fb.ins().ishl_imm(v, width - a);
        self.fb.ins().bor(l, h)
    }

    /// NZCV in nzcv[0..4] as packed n|z|c|v bits (layout matches Cpu).
    fn set_nz(&mut self, v: V, sf: u32) {
        // N = the RESULT's sign bit — bit 31 for a 32-bit op, bit 63 for a
        // 64-bit one (the old code used 63 always, so every 32-bit S op
        // reported N=0).
        let sh = if sf == 0 { 31 } else { 63 };
        let n = self.fb.ins().sshr_imm(v, sh);
        let z0 = self
            .fb
            .ins()
            .icmp_imm(cranelift_codegen::ir::condcodes::IntCC::Equal, v, 0);
        let z = self.bool64(z0);
        self.store_flag(0, n);
        self.store_flag(1, z);
    }

    /// ARM flag semantics for `adds/subs/cmp/cmn a, b` producing result
    /// `v` (adds use `b` as-is; subs pass `b` too — the op distinguishes).
    /// For a 32-bit op the CARRY and OVERFLOW are computed on the 32-bit
    /// operands, so mask them first.
    fn set_flags(&mut self, a: V, b: V, v: V, op: u32, sf: u32) {
        let a = if sf == 0 { self.w32(a) } else { a };
        let b = if sf == 0 { self.w32(b) } else { b };
        self.set_nz(v, sf);
        let sa = self.fb.ins().sshr_imm(a, 63);
        let sb = self.fb.ins().sshr_imm(b, 63);
        let sv = self.fb.ins().sshr_imm(v, 63);
        if op == 0 {
            // ADD: C = result < a (unsigned overflow).
            let c0 = self
                .fb
                .ins()
                .icmp(cranelift_codegen::ir::condcodes::IntCC::UnsignedLessThan, v, a);
            let c = self.bool64(c0);
            self.store_flag(2, c);
            // V = sign(a)==sign(b) && sign(v)!=sign(a).
            let same0 = self
                .fb
                .ins()
                .icmp(cranelift_codegen::ir::condcodes::IntCC::Equal, sa, sb);
            let same = self.bool64(same0);
            let diff0 = self
                .fb
                .ins()
                .icmp(cranelift_codegen::ir::condcodes::IntCC::NotEqual, sv, sa);
            let diff = self.bool64(diff0);
            let ovf = self.fb.ins().band(same, diff);
            self.store_flag(3, ovf);
        } else {
            // SUB: C = a >= b (no borrow).
            let c0 = self
                .fb
                .ins()
                .icmp(cranelift_codegen::ir::condcodes::IntCC::UnsignedGreaterThanOrEqual, a, b);
            let c = self.bool64(c0);
            self.store_flag(2, c);
            // V = sign(a)!=sign(b) && sign(v)!=sign(a).
            let dsn0 = self
                .fb
                .ins()
                .icmp(cranelift_codegen::ir::condcodes::IntCC::NotEqual, sa, sb);
            let dsn = self.bool64(dsn0);
            let dsv0 = self
                .fb
                .ins()
                .icmp(cranelift_codegen::ir::condcodes::IntCC::NotEqual, sv, sa);
            let dsv = self.bool64(dsv0);
            let ovf = self.fb.ins().band(dsn, dsv);
            self.store_flag(3, ovf);
        }
    }

    /// NZCV lives as four separate bytes [n, z, c, v] at `nzvp` (matches
    /// the interpreter's layout). Store a single byte — an I64 store would
    /// clobber the following flags (and, worse, whatever is past a small
    /// caller-side array).
    fn store_flag(&mut self, i: u32, v: V) {
        let b = self.fb.ins().band_imm(v, 1);
        let b8 = self.fb.ins().ireduce(types::I8, b);
        self.fb
            .ins()
            .store(MemFlags::trusted(), b8, self.nzvp, i as i32);
    }

    /// Cranelift `icmp`/`icmp_imm` produce I8 booleans; every flag store
    /// and boolean op here is I64, so zero-extend before use.
    fn bool64(&mut self, v: V) -> V {
        self.fb.ins().uextend(types::I64, v)
    }

    fn load_flag(&mut self, i: u32) -> V {
        let b = self
            .fb
            .ins()
            .load(types::I8, MemFlags::trusted(), self.nzvp, i as i32);
        // Zero-extend to I64: every later use bands/compares against I64.
        self.fb.ins().uextend(types::I64, b)
    }

    fn cond(&mut self, c: u32) -> Option<V> {
        let n = self.load_flag(0);
        let z = self.load_flag(1);
        let cc = self.load_flag(2);
        let v = self.load_flag(3);
        let nz = self.fb.ins().band_not(n, z);
        let one = self.iconst(1);
        let z1 = self.fb.ins().band(z, one);
        let c1 = self.fb.ins().band(cc, one);
        let v1 = self.fb.ins().band(v, one);
        let zero = self.iconst(0);
        let z_not = self.fb.ins().bxor(z1, one);
        let c_not = self.fb.ins().bxor(c1, one);
        let v_not = self.fb.ins().bxor(v1, one);
        let n1 = self.fb.ins().band(n, one);
        let pl = self.fb.ins().bxor(n1, one);
        let ge0 = self
            .fb
            .ins()
            .icmp(cranelift_codegen::ir::condcodes::IntCC::Equal, nz, zero);
        let ge = self.bool64(ge0);
        let lt0 = self
            .fb
            .ins()
            .icmp(cranelift_codegen::ir::condcodes::IntCC::NotEqual, nz, zero);
        let lt = self.bool64(lt0);
        let hi = self.fb.ins().band(c1, z_not);
        let ls = self.fb.ins().bor(c_not, z1);
        let gt = self.fb.ins().band(z_not, ge);
        let le = self.fb.ins().bor(z1, lt);
        let r = match c {
            0 => z1, // eq
            1 => z_not, // ne
            2 => c1, // cs
            3 => c_not, // cc
            4 => n, // mi
            5 => pl, // pl
            6 => v1, // vs
            7 => v_not, // vc
            8 => hi, // hi
            9 => ls, // ls
            10 => ge, // ge
            11 => lt, // lt
            12 => gt, // gt
            13 => le, // le
            _ => return None,
        };
        Some(self.fb.ins().band(r, one))
    }

    fn mem_rd(&mut self, addr: V, bytes: u64) -> Option<V> {
        let out = self.fb.create_sized_stack_slot(cranelift_codegen::ir::StackSlotData::new(
            cranelift_codegen::ir::StackSlotKind::ExplicitSlot,
            8,
            8,
        ));
        let out_addr = self.fb.ins().stack_addr(types::I64, out, 0);
        let rd_ref = self
            .module
            .declare_func_in_func(self.rd_id, self.fb.func);
        let bsz = self.iconst(bytes);
        let call = self
            .fb
            .ins()
            .call(rd_ref, &[self.busp, addr, bsz, out_addr]);
        let status = self.fb.inst_results(call)[0];
        // On fault, branch to the shared fault-exit block (return -1)
        // INSTEAD of continuing with a poisoned register — the
        // interpreter stops at the faulting instruction, and so must we.
        let bad = self
            .fb
            .ins()
            .icmp_imm(cranelift_codegen::ir::condcodes::IntCC::NotEqual, status, 0);
        let cont = self.fb.create_block();
        self.fb.ins().brif(bad, self.fault_exit, &[], cont, &[]);
        self.fb.switch_to_block(cont);
        self.fb.seal_block(cont);
        let good = self
            .fb
            .ins()
            .load(types::I64, MemFlags::trusted(), out_addr, 0);
        Some(good)
    }

    fn mem_wr(&mut self, addr: V, bytes: u64, val: V) -> Option<()> {
        let wr_ref = self
            .module
            .declare_func_in_func(self.wr_id, self.fb.func);
        let bsz = self.iconst(bytes);
        let call = self
            .fb
            .ins()
            .call(wr_ref, &[self.busp, addr, bsz, val]);
        let status = self.fb.inst_results(call)[0];
        let bad = self
            .fb
            .ins()
            .icmp_imm(cranelift_codegen::ir::condcodes::IntCC::NotEqual, status, 0);
        let cont = self.fb.create_block();
        self.fb.ins().brif(bad, self.fault_exit, &[], cont, &[]);
        self.fb.switch_to_block(cont);
        self.fb.seal_block(cont);
        Some(())
    }
}
