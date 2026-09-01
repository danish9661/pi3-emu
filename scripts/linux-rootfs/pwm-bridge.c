/*
 * pwm-bridge: browser <-> BCM2835 PWM bridge device.
 * See hw/misc/pwm-bridge.h for the register map and wiring contract.
 *
 * Emulates the BCM2835 PWM channel 1 in FIFO mode.  Guest FIFO/DAT1
 * writes are forwarded to the browser as "PWM <count>\n" (one message
 * per sync boundary, count = samples drained in that period).
 */

#include "qemu/osdep.h"
#include "qemu/module.h"
#include "qemu/log.h"
#include "hw/sysbus.h"
#include "hw/qdev-properties.h"
#include "hw/misc/pwm-bridge.h"
#include <emscripten.h>

static PWMBridgeState *current_pwm;

/* ---- C -> browser transport ---- */

EM_JS(void, pwm_js_tx, (int len, const char *data), {
    var s = "";
    for (var i = 0; i < len; i++) {
        s += String.fromCharCode(HEAPU8[data + i]);
    }
    if (typeof emscripten_post_message !== 'undefined') {
        emscripten_post_message(s);
    } else if (typeof postMessage !== 'undefined') {
        postMessage(s);
    }
});

static void pwm_bridge_tx_samples(int count)
{
    char buf[32];
    int n = snprintf(buf, sizeof(buf), "PWM %d\n", count);
    pwm_js_tx(n, buf);
}

/* ---- BCM2835 PWM register offsets ---- */

#define PWM_CTL   0x00
#define PWM_STA   0x04
#define PWM_RNG1  0x10
#define PWM_DAT1  0x14
#define PWM_FIFO  0x18

/* CTL bits */
#define CTL_PWEN1  (1 << 0)
#define CTL_MODE1  (1 << 1)
#define CTL_USEF1  (1 << 5)
#define CTL_CLRF1  (1 << 6)
#define CTL_MSEN1  (1 << 7)
#define CTL_LATCH  (CTL_PWEN1 | CTL_MODE1 | CTL_USEF1 | CTL_MSEN1)

/* STA bits */
#define STA_FULL1  (1 << 0)
#define STA_EMPT1  (1 << 1)

#define FIFO_DEPTH 256

/* ---- MMIO read handler ---- */

static uint64_t pwm_bridge_read(void *opaque, hwaddr offset, unsigned size)
{
    PWMBridgeState *s = opaque;

    switch (offset) {
    case PWM_CTL:
        return s->ctl;
    case PWM_STA: {
        uint32_t sta = 0;
        if (s->fifo_len >= FIFO_DEPTH) sta |= STA_FULL1;
        if (s->fifo_len == 0) sta |= STA_EMPT1;
        return sta;
    }
    case PWM_RNG1:
        return s->rng1;
    case PWM_DAT1:
        return s->dat1;
    default:
        return 0;
    }
}

/* ---- MMIO write handler ---- */

static void pwm_bridge_write(void *opaque, hwaddr offset,
                             uint64_t value, unsigned size)
{
    PWMBridgeState *s = opaque;

    switch (offset) {
    case PWM_CTL: {
        uint32_t v = (uint32_t)value;
        if (v & CTL_CLRF1) {
            s->fifo_len = 0;
        }
        s->ctl = v & CTL_LATCH;
        break;
    }
    case PWM_RNG1:
        s->rng1 = (uint32_t)value;
        break;
    case PWM_DAT1:
        s->dat1 = (uint32_t)value;
        if ((s->ctl & CTL_USEF1) && s->fifo_len < FIFO_DEPTH) {
            s->fifo[s->fifo_len++] = (uint32_t)value;
        }
        break;
    case PWM_FIFO:
        if (s->fifo_len < FIFO_DEPTH) {
            s->fifo[s->fifo_len++] = (uint32_t)value;
        }
        break;
    default:
        break;
    }
}

/* ---- Sync: drain FIFO and forward to browser ---- */

static void pwm_bridge_sync(PWMBridgeState *s)
{
    int drain = s->fifo_len > 0 ? (s->fifo_len < 64 ? s->fifo_len : 64) : 0;
    if (drain > 0) {
        /* Shift drained samples out of the FIFO */
        s->fifo_len -= drain;
        if (s->fifo_len > 0) {
            memmove(s->fifo, s->fifo + drain, s->fifo_len * sizeof(uint32_t));
        }
        s->samples_drained += drain;
        /* Forward to browser */
        pwm_bridge_tx_samples(drain);
    }
}

/* ---- MemoryRegion ops ---- */

static const MemoryRegionOps pwm_bridge_ops = {
    .read = pwm_bridge_read,
    .write = pwm_bridge_write,
    .endianness = DEVICE_NATIVE_ENDIAN,
    .impl.min_access_size = 4,
    .impl.max_access_size = 4,
};

/* ---- Device realize ---- */

static void pwm_bridge_realize(DeviceState *dev, Error **errp)
{
    PWMBridgeState *s = PWM_BRIDGE(dev);
    current_pwm = s;
    memory_region_init_io(&s->mmio, OBJECT(dev), &pwm_bridge_ops,
                          s, TYPE_PWM_BRIDGE, PWM_BRIDGE_SIZE);
    sysbus_init_child_obj(OBJECT(dev), "pwm-bridge-mmio", &s->mmio,
                          sizeof(s->mmio), &memory_region_ops);
    sysbus_init_mmio(SYS_BUS_DEVICE(dev), &s->mmio);
}

/* ---- Device reset ---- */

static void pwm_bridge_reset(DeviceState *dev)
{
    PWMBridgeState *s = PWM_BRIDGE(dev);
    s->ctl = 0;
    s->rng1 = 0;
    s->dat1 = 0;
    s->fifo_len = 0;
    s->sta = 0;
    s->samples_drained = 0;
}

/* ---- Class init ---- */

static void pwm_bridge_class_init(ObjectClass *klass, void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    dc->realize = pwm_bridge_realize;
    dc->reset = pwm_bridge_reset;
}

static const TypeInfo pwm_bridge_info = {
    .name = TYPE_PWM_BRIDGE,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(PWMBridgeState),
    .class_init = pwm_bridge_class_init,
};

static void pwm_bridge_register_types(void)
{
    type_register_static(&pwm_bridge_info);
}

type_init(pwm_bridge_register_types)
