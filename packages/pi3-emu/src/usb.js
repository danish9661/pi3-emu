// Synopsys DesignWare USB 2.0 OTG controller (DWC2) at 0x3F980000.
//
// Full OTG PHY emulation for Linux dwc2 driver probe without blacklist.
// Based on QEMU hw/usb/hcd-dwc2.c reset values and Linux dwc2/hw.h defs.
// - GHWCFG1-4, GSNPSID, GUID, GPVNDCTL, GGPIO mirror QEMU defaults
// - GOTGCTL OTG PHY state (VBUS, CONID_B, SESREQ, HNP) with debounces
// - GINTSTS/GINTMSK interrupt aggregation, VBUS/OTGIRQ generation
// - GRXFSIZ/GNPTXFSIZ/HPTXFSIZ FIFO sizing
// - Host mode detection (CURMOD_HOST) and SOF tick
// Performance: write-once ID regs + dirty-flag gating, hook on 64K window.

import { readU32, writeU32 } from './perf.js';

export function createUsb(uc, ucMod, base) {
  const GOTGCTL  = base + 0x000;
  const GOTGINT  = base + 0x004;
  const GAHBCFG  = base + 0x008;
  const GUSBCFG  = base + 0x00C;
  const GRSTCTL  = base + 0x010;
  const GINTSTS  = base + 0x014;
  const GINTMSK  = base + 0x018;
  const GRXSTSP  = base + 0x01C;
  const GRXSTSR  = base + 0x01C; // same addr, read-only status
  const GRXFSIZ  = base + 0x024;
  const GNPTXFSIZ= base + 0x028;
  const GNPTXSTS = base + 0x02C;
  const GI2CCTL  = base + 0x030;
  const GPVNDCTL = base + 0x034;
  const GGPIO    = base + 0x038;
  const GUID     = base + 0x03C;
  const GSNPSID  = base + 0x040;
  const GHWCFG1  = base + 0x044;
  const GHWCFG2  = base + 0x048;
  const GHWCFG3  = base + 0x04C;
  const GHWCFG4  = base + 0x050;
  const GLPMCFG  = base + 0x054;
  const GPWRDN   = base + 0x058;
  const GDFIFOCFG= base + 0x05C;
  const GADPCTL  = base + 0x060;
  const HPTXFSIZ = base + 0x100;
  const HCFG     = base + 0x400;
  const HFIR     = base + 0x404;
  const HFNUM    = base + 0x408;
  const HPTXSTS  = base + 0x410;
  const HAINT    = base + 0x414;
  const HAINTMSK = base + 0x418;
  const HPRT     = base + 0x440;

  // --- bit defs (from dwc2-regs.h) ---
  const GOTGCTL_BSESVLD = 1 << 19;
  const GOTGCTL_ASESVLD = 1 << 18;
  const GOTGCTL_CONID_B = 1 << 16;
  const GOTGCTL_SESREQ  = 1 << 1;
  const GOTGCTL_SESREQSCS = 1 << 0;
  const GOTGCTL_HSTNEGSCS = 1 << 8;
  const GOTGCTL_HNPREQ = 1 << 9;

  const GOTGINT_SES_REQ_SUC_STS_CHNG = 1 << 8;
  const GOTGINT_HST_NEG_DET = 1 << 17;

  const GINTSTS_CURMODE_HOST = 1 << 0;
  const GINTSTS_MODEMIS = 1 << 1;
  const GINTSTS_OTGINT = 1 << 2;
  const GINTSTS_SOF = 1 << 3;
  const GINTSTS_RXFLVL = 1 << 4;
  const GINTSTS_NPTXFEMP = 1 << 5;
  const GINTSTS_PTXFEMP = 1 << 26;
  const GINTSTS_HCHINT = 1 << 25;
  const GINTSTS_PRTINT = 1 << 24;
  const GINTSTS_CONIDSTSCHNG = 1 << 28;

  const GRSTCTL_AHBIDLE = 1 << 31;
  const GRSTCTL_CSFTRST = 1 << 0;
  const GRSTCTL_HSFTRST = 1 << 1;
  const GRSTCTL_RXFFLSH = 1 << 4;
  const GRSTCTL_TXFFLSH = 1 << 5;

  const GAHBCFG_GLBL_INTR_EN = 1 << 0;

  // GHWCFG2: host-only, internal DMA, 16 host channels (example: 16-1=15)
  // 8 token q depth, 4 host perio tx q depth, 4 nonperio, dynamic fifo, perio ep
  const DWC2_NB_CHAN = 16;
  const GHWCFG2_VAL = (8 << 26) | (4 << 24) | (4 << 22) | (1 << 19) | (1 << 18)
                    | ((DWC2_NB_CHAN - 1) << 14) | (2 << 3) | (6 << 0); // 6 = no-SRP host
  const GHWCFG3_VAL = (4096 << 16) | (4 << 4) | (4 << 0);
  const GSNPSID_VAL = 0x4f54294a; // QEMU hcd-dwc2: 4.20a

  const state = {
    enabled: false,
    frameNum: 0,
    inited: false,
    touched: false,
    sofTicks: 0,
    // shadow regs
    gotgctl: GOTGCTL_BSESVLD | GOTGCTL_ASESVLD | GOTGCTL_CONID_B,
    gotgint: 0,
    gahbcfg: 0,
    gusbcfg: 5 << 10, // USBTRDTIM=5
    grstctl: GRSTCTL_AHBIDLE,
    gintsts: GINTSTS_CONIDSTSCHNG | GINTSTS_PTXFEMP | GINTSTS_NPTXFEMP | GINTSTS_CURMODE_HOST,
    gintmsk: 0,
    grxfsiz: 1024,
    gnptxfsiz: 1024 << 16, // depth 1024 at top
    gnptxsts: (4 << 16) | 1024, // from QEMU: 4 q entries, 1024 fifo-space
    hptxfsiz: 500 << 16,
    hcfg: 2 << 1, // resvalid 2
    hfir: 60000,
    hprt: 0,
    gintmsk2: 0,
    // VBUS/session state
    vbusValid: true,
    sessReqInProg: false,
    hnpReqInProg: false,
  };

  function initRegs(uc) {
    writeU32(uc, GSNPSID, GSNPSID_VAL);
    writeU32(uc, GUID, 0);
    writeU32(uc, GHWCFG1, 0);
    writeU32(uc, GHWCFG2, GHWCFG2_VAL);
    writeU32(uc, GHWCFG3, GHWCFG3_VAL);
    writeU32(uc, GHWCFG4, 0);
    writeU32(uc, GOTGCTL, state.gotgctl);
    writeU32(uc, GOTGINT, 0);
    writeU32(uc, GAHBCFG, 0);
    writeU32(uc, GUSBCFG, state.gusbcfg);
    writeU32(uc, GRSTCTL, state.grstctl);
    writeU32(uc, GINTSTS, state.gintsts);
    writeU32(uc, GINTMSK, 0);
    writeU32(uc, GRXSTSP, 0);
    writeU32(uc, GRXFSIZ, state.grxfsiz);
    writeU32(uc, GNPTXFSIZ, state.gnptxfsiz);
    writeU32(uc, GNPTXSTS, state.gnptxsts);
    writeU32(uc, GI2CCTL, (1 << 28) | (1 << 24)); // I2CDATSE0 | ACK
    writeU32(uc, GPVNDCTL, 0);
    writeU32(uc, GGPIO, 0);
    writeU32(uc, GLPMCFG, 0);
    writeU32(uc, GPWRDN, 1 << 0); // PWRDNRSTN
    writeU32(uc, GDFIFOCFG, 0);
    writeU32(uc, GADPCTL, 0);
    writeU32(uc, HPTXFSIZ, state.hptxfsiz);
    writeU32(uc, HCFG, state.hcfg);
    writeU32(uc, HFIR, state.hfir);
    writeU32(uc, HFNUM, 0x3fff);
    writeU32(uc, HPTXSTS, (16 << 16) | 32768);
    writeU32(uc, HAINT, 0);
    writeU32(uc, HAINTMSK, 0);
    writeU32(uc, HPRT, 0);
    state.inited = true;
  }

  function recomputeGintsts() {
    // OTGIRQ if GOTGINT & non-zero, plus SOF throttled, etc.
    let sts = state.gintsts;
    if (state.gotgint !== 0) sts |= GINTSTS_OTGINT;
    else sts &= ~GINTSTS_OTGINT;
    // NPTX/PTX fifo always empty (we have infinite space)
    sts |= GINTSTS_NPTXFEMP | GINTSTS_PTXFEMP;
    sts |= GINTSTS_CURMODE_HOST;
    state.gintsts = sts;
  }

  function syncOut(uc) {
    if (!state.inited) initRegs(uc);
    // SOF ticker: increment HFNUM and pulse GINTSTS_SOF every 8 ticks (~1ms at 19.2MHz)
    if (state.enabled || state.touched) {
      state.frameNum = (state.frameNum + 1) & 0x3fff;
      writeU32(uc, HFNUM, state.frameNum);
      state.sofTicks++;
      if ((state.sofTicks & 7) === 0) {
        state.gintsts |= GINTSTS_SOF;
      }
      recomputeGintsts();
      writeU32(uc, GINTSTS, state.gintsts);
      writeU32(uc, GOTGCTL, state.gotgctl);
      writeU32(uc, GOTGINT, state.gotgint);
    }
    state.touched = false;
  }

  function syncIn(uc) {
    // Read back guest writes
    const hcfg = readU32(uc, HCFG);
    const wasEnabled = state.enabled;
    // HCFG bit0 not used; use FSLSPCS etc. For our model, any non-zero HCFG enables host
    // but we gate on the DWC2 being in host mode (CURMODE_HOST). Keep simple: enabled if GAHBCFG DMA_EN?
    const gahbcfg = readU32(uc, GAHBCFG);
    const gusbcfg = readU32(uc, GUSBCFG);
    const gotgctl = readU32(uc, GOTGCTL);
    const grstctl = readU32(uc, GRSTCTL);
    const gintmsk = readU32(uc, GINTMSK);
    const gotgint = readU32(uc, GOTGINT);

    state.gahbcfg = gahbcfg;
    state.gusbcfg = gusbcfg;
    state.gintmsk = gintmsk;

    // Handle soft reset: CSFTRST / HSFTRST -> pulse AHBIDLE
    if (grstctl & (GRSTCTL_CSFTRST | GRSTCTL_HSFTRST)) {
      // Guest requests core soft reset: clear FIFOs, reset GINTSTS
      state.gintsts = GINTSTS_CONIDSTSCHNG | GINTSTS_PTXFEMP | GINTSTS_NPTXFEMP | GINTSTS_CURMODE_HOST;
      state.gotgint = 0;
      state.gotgctl = GOTGCTL_BSESVLD | GOTGCTL_ASESVLD | GOTGCTL_CONID_B;
      // AHBIDLE goes low during reset then high
      writeU32(uc, GRSTCTL, GRSTCTL_AHBIDLE);
      state.touched = true;
      return;
    }
    if (grstctl & GRSTCTL_RXFFLSH) {
      // RX flush -> clear RX level
      state.gintsts &= ~GINTSTS_RXFLVL;
      writeU32(uc, GRSTCTL, GRSTCTL_AHBIDLE);
    }
    if (grstctl & GRSTCTL_TXFFLSH) {
      state.gintsts |= GINTSTS_NPTXFEMP | GINTSTS_PTXFEMP;
      writeU32(uc, GRSTCTL, GRSTCTL_AHBIDLE);
    }

    // GOTGCTL SESREQ handling (OTG B-device session request)
    const sesReqNow = (gotgctl & GOTGCTL_SESREQ) !== 0;
    const sesReqScsNow = (gotgctl & GOTGCTL_SESREQSCS) !== 0;
    if (sesReqNow && !state.sessReqInProg) {
      state.sessReqInProg = true;
      // Simulate session valid becoming true, and success status
      state.gotgctl |= GOTGCTL_SESREQSCS | GOTGCTL_BSESVLD | GOTGCTL_ASESVLD;
      state.gotgint |= GOTGINT_SES_REQ_SUC_STS_CHNG;
      state.gintsts |= GINTSTS_OTGINT;
      state.touched = true;
    } else if (!sesReqNow && state.sessReqInProg) {
      state.sessReqInProg = false;
      state.gotgctl &= ~GOTGCTL_SESREQSCS;
      // no interrupt on deassert
    }
    // HNP handling
    const hnpReqNow = (gotgctl & GOTGCTL_HNPREQ) !== 0;
    if (hnpReqNow && !state.hnpReqInProg) {
      state.hnpReqInProg = true;
      state.gotgctl |= GOTGCTL_HSTNEGSCS;
      state.gotgint |= GOTGINT_HST_NEG_DET;
      state.gintsts |= GINTSTS_OTGINT;
      state.touched = true;
    } else if (!hnpReqNow && state.hnpReqInProg) {
      state.hnpReqInProg = false;
      state.gotgctl &= ~GOTGCTL_HSTNEGSCS;
    }

    // W1C for GOTGINT and GINTSTS: guest writes 1 to clear
    if (gotgint !== state.gotgint) {
      // Guest W1C: clear bits it wrote 1 to
      state.gotgint &= ~gotgint;
      if (state.gotgint === 0) state.gintsts &= ~GINTSTS_OTGINT;
    }
    const gintstsWr = readU32(uc, GINTSTS);
    // W1C for GINTSTS: bits written 1 get cleared (except RO)
    const w1cMask = gintstsWr & (GINTSTS_SOF | GINTSTS_PRTINT | GINTSTS_HCHINT | GINTSTS_CONIDSTSCHNG | GINTSTS_OTGINT);
    if (w1cMask) {
      state.gintsts &= ~w1cMask;
      writeU32(uc, GINTSTS, state.gintsts);
    }

    // HPRT connection: if guest sets PPWR, show connected; host always VBUS valid
    const hprt = readU32(uc, HPRT);
    if (hprt & (1 << 12)) { // PPWR
      // keep connected: set PRTConnSts = bit0, ENA = bit2 when powered
      state.hprt = hprt | (1 << 0);
      if (hprt & (1 << 2)) state.gintsts |= GINTSTS_PRTINT;
    } else {
      state.hprt = hprt & ~(1 << 0);
    }

    // Enable if global intr enable + host mode; used for HFNUM tick gating
    const enabledNow = (gahbcfg & GAHBCFG_GLBL_INTR_EN) !== 0;
    if (enabledNow !== wasEnabled) state.touched = true;
    state.enabled = enabledNow;

    recomputeGintsts();
  }

  // Mark dirty when guest writes any register in our 64K window
  uc.hook_add(
    ucMod.HOOK_MEM_WRITE,
    () => { state.touched = true; },
    null,
    base,
    base + 0x10000 - 1
  );

  return { state, syncOut, syncIn };
}
