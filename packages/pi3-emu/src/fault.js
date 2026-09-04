// Guest fault decoder: turn a raw uc_emu_start failure into a human-
// readable crash report (PC/SP/instruction/registers/likely cause).
// Firmware developers — including beginners on a Wokwi-style site — get
// "PC 0x1001a4 executed unmapped memory" instead of UC_ERR_* and silence.
//
// This unicorn build does not report the faulting data address, so data
// aborts are attributed by PC + opcode; that limitation is stated in the
// report when it applies.

export function decodeFault(uc, ucMod, err, opts = {}) {
  const ramBase = opts.ramBase ?? 0x0;
  const ramSize = opts.ramSize ?? 0x400000;
  const out = { pc: 0, sp: 0, insn: null, regs: {}, raw: '', cause: '' };
  try {
    out.raw = String((err && (err.message || err)) || err);
  } catch (_) {
    out.raw = 'emu error';
  }
  const tryReg = (id) => {
    try {
      const v = Number(uc.reg_read_i32(id));
      return Number.isFinite(v) ? v >>> 0 : null;
    } catch (_) {
      return null;
    }
  };
  out.pc = tryReg(ucMod.ARM64_REG_PC);
  out.sp = tryReg(ucMod.ARM64_REG_SP);
  for (let i = 0; i < 8; i++) {
    try {
      const v = Number(uc.reg_read_i32(ucMod['ARM64_REG_X' + i]));
      if (Number.isFinite(v)) out.regs['x' + i] = v >>> 0;
    } catch (_) {}
  }
  const extra = ['ELR_EL1', 'SPSR_EL1', 'ESR_EL1', 'VBAR_EL1', 'FAR_EL1'];
  for (const r of extra) {
    try {
      const v = Number(uc.reg_read_i32(ucMod['ARM64_REG_' + r]));
      if (Number.isFinite(v)) out.regs[r.toLowerCase()] = v >>> 0;
    } catch (_) {}
  }
  if (out.pc !== null) {
    try {
      const b = uc.mem_read(out.pc, 4);
      out.insn = '0x' + [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
    } catch (_) {
      out.insn = null;
    }
  }
  const hex = (v) => (v === null ? '?' : '0x' + v.toString(16));
  if (out.pc === null || out.pc === 0) {
    out.cause = 'PC is zero/unreadable (bad vector, clobbered x30, or uninitialized entry)';
  } else if (out.pc < ramBase || out.pc >= ramBase + ramSize) {
    const inMmio =
      (out.pc >= 0x3f000000 && out.pc < 0x41000000) ? ' (inside the MMIO region — executed data?)' : '';
    out.cause = `PC ${hex(out.pc)} is outside RAM [${hex(ramBase)}..${hex(ramBase + ramSize)})${inMmio}`;
  } else if (out.insn === null) {
    out.cause = `PC ${hex(out.pc)} is in RAM but unreadable (partial mapping?)`;
  } else {
    out.cause =
      `fault at PC ${hex(out.pc)} (insn ${out.insn}); ` +
      'data-abort address is not reported by this core build — ' +
      'suspect the load/store in the faulting instruction';
  }
  out.message =
    `guest fault: ${out.cause}\n` +
    `  pc=${hex(out.pc)} sp=${hex(out.sp)} raw=[${out.raw.slice(0, 160)}]`;
  return out;
}
