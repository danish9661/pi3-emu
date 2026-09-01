/*
 * pwm-bridge: browser <-> BCM2835 PWM bridge device.
 *
 * Emulates the BCM2835 PWM registers at 0x3F20C000 and forwards FIFO
 * samples to the browser via emscripten postMessage.  The browser can
 * observe the stream of samples for visualisation or recording.
 *
 * Register map (BCM2835 PWM, channel 1):
 *   +0x00 CTL   control (PWEN1, MODE1, USEF1, CLRF1, MSEN1)
 *   +0x04 STA   status (FULL1, EMPT1)
 *   +0x08 DMAC  DMA control
 *   +0x0C reserved
 *   +0x10 RNG1  range (number of cycles per sample)
 *   +0x14 DAT1  data (direct-push sample)
 *   +0x18 FIFO  FIFO push (write-only)
 *
 * Guest FIFO/DAT1 writes are forwarded to the browser as
 * "PWM <count>\n" (one message per sync, count = samples drained).
 *
 * C<->browser transport: emscripten postMessage (worker -> main thread)
 * and a shared pi3_rx function (main thread -> worker).
 */

#ifndef PWM_BRIDGE_H
#define PWM_BRIDGE_H

#include "hw/sysbus.h"
#include "qom/object.h"

#define TYPE_PWM_BRIDGE "pwm-bridge"
#define PWM_BRIDGE_SIZE 0x1000

struct PWMBridgeState {
    SysBusDevice parent_obj;
    MemoryRegion mmio;
    uint32_t ctl;       /* CTL register (latched bits) */
    uint32_t rng1;      /* RNG1 register */
    uint32_t dat1;      /* DAT1 register */
    uint32_t fifo[256]; /* FIFO buffer */
    int fifo_len;       /* current FIFO depth */
    uint32_t sta;       /* status */
    int samples_drained; /* samples drained since last sync (browser msg) */
};
typedef struct PWMBridgeState PWMBridgeState;

OBJECT_DECLARE_SIMPLE_TYPE(PWMBridgeState, PWM_BRIDGE)

#endif /* PWM_BRIDGE_H */
