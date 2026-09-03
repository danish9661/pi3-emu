/*
 * i2c-bridge: browser <-> BCM2835 I2C (BSC) bridge device.
 * See hw/misc/i2c-bridge.h for the register map and wiring contract.
 *
 * Emulates the BCM2835 BSC master.  Guest transfers are forwarded to
 * the browser as "I2C_TX/WR/RD" messages.  The browser can respond to
 * reads with "I2C_RX <hex bytes>\n" via the shared pi3_rx function.
 */

#include "qemu/osdep.h"
#include "qemu/module.h"
#include "qemu/log.h"
#include "hw/sysbus.h"
#include "hw/qdev-properties.h"
#include "hw/misc/i2c-bridge.h"
#include <emscripten.h>

static I2CBridgeState *current_i2c;

/* ---- C -> browser transport ---- */

EM_JS(void, i2c_js_tx, (int len, const char *data), {
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

static void i2c_bridge_tx_msg(const char *tag, uint32_t addr,
                               uint8_t reg, const uint8_t *data, int len)
{
    char buf[256];
    int pos = 0;
    pos += snprintf(buf + pos, sizeof(buf) - pos, "%s %02x %02x", tag, addr, reg);
    for (int i = 0; i < len && pos < (int)sizeof(buf) - 4; i++) {
        pos += snprintf(buf + pos, sizeof(buf) - pos, " %02x", data[i]);
    }
    pos += snprintf(buf + pos, sizeof(buf) - pos, "\n");
    i2c_js_tx(pos, buf);
}

/* ---- Browser -> device: accept response bytes for a pending read ---- */

EMSCRIPTEN_KEEPALIVE
void i2c_bridge_rx(const char *hex)
{
    I2CBridgeState *s = current_i2c;
    if (!s) return;
    /* Parse "I2C_RX XX YY ZZ" */
    s->resp_len = 0;
    s->resp_pos = 0;
    while (*hex && s->resp_len < I2C_FIFO_DEPTH) {
        unsigned byte;
        if (sscanf(hex, " %2x", &byte) == 1) {
            s->resp[s->resp_len++] = (uint8_t)byte;
            hex += 3;
        } else {
            break;
        }
    }
}

/* ---- BCM2835 BSC register offsets ---- */

#define I2C_C    0x00
#define I2C_S    0x04
#define I2C_DLEN 0x08
#define I2C_A    0x0C
#define I2C_FIFO 0x10
#define I2C_DIV  0x14

/* C bits */
#define C_I2CEN  (1 << 15)
#define C_ST     (1 << 7)
#define C_CLEAR  (1 << 4)
#define C_READ   (1 << 0)

/* S bits */
#define S_DONE   (1 << 7)
#define S_RXF    (1 << 6)
#define S_TXE    (1 << 4)

/* ---- MMIO read handler ---- */

static uint64_t i2c_bridge_read(void *opaque, hwaddr offset, unsigned size)
{
    I2CBridgeState *s = opaque;

    switch (offset) {
    case I2C_C:
        return s->c | (s->s & S_DONE ? C_ST : 0);
    case I2C_S:
        return s->s;
    case I2C_DLEN:
        return s->dlen;
    case I2C_A:
        return s->addr;
    case I2C_FIFO: {
        /* Pop a byte from the response FIFO */
        if (s->resp_pos < s->resp_len) {
            return s->resp[s->resp_pos++];
        }
        return 0;
    }
    case I2C_DIV:
        return s->div;
    default:
        return 0;
    }
}

/* ---- MMIO write handler ---- */

static void i2c_bridge_write(void *opaque, hwaddr offset,
                             uint64_t value, unsigned size)
{
    I2CBridgeState *s = opaque;

    switch (offset) {
    case I2C_C: {
        uint32_t v = (uint32_t)value;
        if (v & C_CLEAR) {
            s->s = 0;
            s->fifo_len = 0;
            s->resp_len = 0;
            s->resp_pos = 0;
            s->c &= ~(C_ST | C_READ);
            break;
        }
        /* Detect ST rising edge (start condition) */
        if ((v & C_ST) && !(s->c & C_ST)) {
            s->c |= C_ST;
            s->c = (s->c & ~C_ST) | C_I2CEN | (v & C_READ);
            s->addr = 0; /* will be latched from the address register */

            if (v & C_READ) {
                /* Read transfer: forward request to browser, provide response */
                i2c_bridge_tx_msg("I2C_RD", s->addr, s->reg,
                                  NULL, s->dlen & 0xffff);
                /* If browser already provided a response, use it */
                if (s->resp_len > 0) {
                    s->s |= S_DONE;
                }
            } else {
                /* Write transfer: latch the data from FIFO */
                s->s |= S_DONE;
                if (s->fifo_len > 0) {
                    i2c_bridge_tx_msg("I2C_TX", s->addr, s->reg,
                                      s->fifo, s->fifo_len);
                    s->reg = s->fifo[0];
                    s->fifo_len = 0;
                }
            }
        } else if (!(v & C_ST)) {
            s->c &= ~C_ST;
        } else {
            s->c = (s->c & ~(C_I2CEN | C_READ)) | (v & (C_I2CEN | C_READ));
        }
        break;
    }
    case I2C_S:
        /* Writing 1 clears DONE */
        if (value & S_DONE) {
            s->s &= ~S_DONE;
        }
        break;
    case I2C_DLEN:
        s->dlen = (uint32_t)value;
        break;
    case I2C_A:
        s->addr = (uint32_t)value & 0x7f;
        break;
    case I2C_FIFO: {
        uint8_t b = (uint8_t)value;
        if (s->fifo_len < I2C_FIFO_DEPTH) {
            s->fifo[s->fifo_len++] = b;
        }
        /* First written byte is the register address */
        if (s->fifo_len == 1) {
            s->reg = b;
        }
        break;
    }
    case I2C_DIV:
        s->div = (uint32_t)value;
        break;
    default:
        break;
    }
}

/* ---- MemoryRegion ops ---- */

static const MemoryRegionOps i2c_bridge_ops = {
    .read = i2c_bridge_read,
    .write = i2c_bridge_write,
    .endianness = DEVICE_NATIVE_ENDIAN,
    .impl.min_access_size = 4,
    .impl.max_access_size = 4,
};

/* ---- Device realize ---- */

static void i2c_bridge_realize(DeviceState *dev, Error **errp)
{
    I2CBridgeState *s = I2C_BRIDGE(dev);
    current_i2c = s;
    memory_region_init_io(&s->mmio, OBJECT(dev), &i2c_bridge_ops,
                          s, TYPE_I2C_BRIDGE, I2C_BRIDGE_SIZE);
    sysbus_init_mmio(SYS_BUS_DEVICE(dev), &s->mmio);
}

/* ---- Device reset ---- */

static void i2c_bridge_reset(DeviceState *dev)
{
    I2CBridgeState *s = I2C_BRIDGE(dev);
    s->c = 0;
    s->s = 0;
    s->dlen = 0;
    s->addr = 0;
    s->div = 0;
    s->fifo_len = 0;
    s->resp_len = 0;
    s->resp_pos = 0;
    s->reg = 0;
}

/* ---- Class init ---- */

static void i2c_bridge_class_init(ObjectClass *klass, void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    dc->realize = i2c_bridge_realize;
    dc->reset = i2c_bridge_reset;
}

static const TypeInfo i2c_bridge_info = {
    .name = TYPE_I2C_BRIDGE,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(I2CBridgeState),
    .class_init = i2c_bridge_class_init,
};

static void i2c_bridge_register_types(void)
{
    type_register_static(&i2c_bridge_info);
}

type_init(i2c_bridge_register_types)
