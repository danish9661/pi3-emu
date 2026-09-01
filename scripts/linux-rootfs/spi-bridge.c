/*
 * spi-bridge: browser <-> BCM2835 SPI0 bridge device.
 * See hw/misc/spi-bridge.h for the register map and wiring contract.
 *
 * Emulates the BCM2835 SPI0 master in single-bit mode.  Guest TX byte
 * writes are forwarded to the browser as "SPI_TX <hex bytes>\n" when
 * the transfer is triggered (TA rises).  The browser can respond with
 * MISO bytes via the shared pi3_rx function ("SPI_RX <hex bytes>\n").
 */

#include "qemu/osdep.h"
#include "qemu/module.h"
#include "qemu/log.h"
#include "hw/sysbus.h"
#include "hw/qdev-properties.h"
#include "hw/misc/spi-bridge.h"
#include <emscripten.h>

static SPIBridgeState *current_spi;

/* ---- C -> browser transport ---- */

EM_JS(void, spi_js_tx, (int len, const char *data), {
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

static void spi_bridge_tx_bytes(const uint8_t *data, int len)
{
    /* Build "SPI_TX XX YY ZZ\n" */
    char buf[256];
    int pos = 0;
    pos += snprintf(buf + pos, sizeof(buf) - pos, "SPI_TX");
    for (int i = 0; i < len && pos < (int)sizeof(buf) - 4; i++) {
        pos += snprintf(buf + pos, sizeof(buf) - pos, " %02x", data[i]);
    }
    pos += snprintf(buf + pos, sizeof(buf) - pos, "\n");
    spi_js_tx(pos, buf);
}

/* ---- Browser -> device: accept MISO bytes ---- */

EMSCRIPTEN_KEEPALIVE
void spi_bridge_rx(const char *hex)
{
    SPIBridgeState *s = current_spi;
    if (!s) return;
    /* Parse "SPI_RX XX YY ZZ" */
    s->rx_len = 0;
    s->rx_pos = 0;
    while (*hex && s->rx_len < SPI_FIFO_DEPTH) {
        unsigned byte;
        if (sscanf(hex, " %2x", &byte) == 1) {
            s->rx_fifo[s->rx_len++] = (uint8_t)byte;
            hex += 3; /* skip " XX" */
        } else {
            break;
        }
    }
}

/* ---- BCM2835 SPI register offsets ---- */

#define SPI_CS    0x00
#define SPI_FIFO  0x04
#define SPI_CLK   0x08
#define SPI_DLEN  0x0C

/* CS bits */
#define CS_CSPOL  (1 << 0)
#define CS_CSPOL0 (1 << 21)
#define CS_TA     (1 << 7)
#define CS_DMAEN  (1 << 8)
#define CS_DMAACTIVE (1 << 9)
#define CS_CLEAR_RX (1 << 5)
#define CS_CLEAR_TX (1 << 4)
#define CS_CLEAR  (3 << 4)
#define CS_DONE   (1 << 16)
#define CS_RXD    (1 << 17)
#define CS_TXD    (1 << 18)
#define CS_RXF    (1 << 19)
#define CS_TXE    (1 << 20)

/* ---- MMIO read handler ---- */

static uint64_t spi_bridge_read(void *opaque, hwaddr offset, unsigned size)
{
    SPIBridgeState *s = opaque;

    switch (offset) {
    case SPI_CS: {
        uint32_t cs = s->cs & (CS_TA | CS_CSPOL | CS_CSPOL0);
        if (s->tx_len > 0 && (s->cs & CS_TA)) {
            cs |= CS_DONE;
        }
        if (s->rx_len > s->rx_pos) {
            cs |= CS_RXD;
        } else {
            cs |= CS_TXD; /* TX always ready (we drain instantly) */
        }
        return cs;
    }
    case SPI_FIFO: {
        /* Pop a byte from the RX FIFO */
        if (s->rx_pos < s->rx_len) {
            return s->rx_fifo[s->rx_pos++];
        }
        return 0xff;
    }
    case SPI_CLK:
        return s->clk;
    case SPI_DLEN:
        return s->dlen;
    default:
        return 0;
    }
}

/* ---- MMIO write handler ---- */

static void spi_bridge_write(void *opaque, hwaddr offset,
                             uint64_t value, unsigned size)
{
    SPIBridgeState *s = opaque;

    switch (offset) {
    case SPI_CS: {
        uint32_t v = (uint32_t)value;
        if (v & CS_CLEAR) {
            s->tx_len = 0;
            s->rx_len = 0;
            s->rx_pos = 0;
        }
        if (v & CS_TA) {
            if (!(s->cs & CS_TA)) {
                /* TA rising edge: transfer starts */
                s->cs |= CS_TA;
                if (s->tx_len > 0) {
                    spi_bridge_tx_bytes(s->tx_fifo, s->tx_len);
                    /* Mark done — the browser can respond asynchronously
                     * via spi_bridge_rx(); if no response yet, the guest
                     * sees RXD cleared. */
                }
            }
        } else {
            s->cs &= ~CS_TA;
        }
        break;
    }
    case SPI_FIFO: {
        uint8_t b = (uint8_t)value;
        if (s->tx_len < SPI_FIFO_DEPTH) {
            s->tx_fifo[s->tx_len++] = b;
        }
        break;
    }
    case SPI_CLK:
        s->clk = (uint32_t)value;
        break;
    case SPI_DLEN:
        s->dlen = (uint32_t)value;
        break;
    default:
        break;
    }
}

/* ---- MemoryRegion ops ---- */

static const MemoryRegionOps spi_bridge_ops = {
    .read = spi_bridge_read,
    .write = spi_bridge_write,
    .endianness = DEVICE_NATIVE_ENDIAN,
    .impl.min_access_size = 4,
    .impl.max_access_size = 4,
};

/* ---- Device realize ---- */

static void spi_bridge_realize(DeviceState *dev, Error **errp)
{
    SPIBridgeState *s = SPI_BRIDGE(dev);
    current_spi = s;
    memory_region_init_io(&s->mmio, OBJECT(dev), &spi_bridge_ops,
                          s, TYPE_SPI_BRIDGE, SPI_BRIDGE_SIZE);
    sysbus_init_child_obj(OBJECT(dev), "spi-bridge-mmio", &s->mmio,
                          sizeof(s->mmio), &spi_bridge_ops);
    sysbus_init_mmio(SYS_BUS_DEVICE(dev), &s->mmio);
}

/* ---- Device reset ---- */

static void spi_bridge_reset(DeviceState *dev)
{
    SPIBridgeState *s = SPI_BRIDGE(dev);
    s->cs = 0;
    s->clk = 0;
    s->dlen = 0;
    s->tx_len = 0;
    s->rx_len = 0;
    s->rx_pos = 0;
}

/* ---- Class init ---- */

static void spi_bridge_class_init(ObjectClass *klass, void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    dc->realize = spi_bridge_realize;
    dc->reset = spi_bridge_reset;
}

static const TypeInfo spi_bridge_info = {
    .name = TYPE_SPI_BRIDGE,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(SPIBridgeState),
    .class_init = spi_bridge_class_init,
};

static void spi_bridge_register_types(void)
{
    type_register_static(&spi_bridge_info);
}

type_init(spi_bridge_register_types)
