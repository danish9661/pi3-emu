/*
 * spi-bridge: browser <-> BCM2835 SPI0 bridge device.
 * See hw/misc/spi-bridge.h for the register map and wiring contract.
 *
 * Emulates the BCM2835 SPI0 master registers at 0x3F204000.  Guest TX
 * FIFO writes are forwarded to the browser as "SPI_TX <hex bytes>\n".
 * The browser can respond by driving the RX FIFO with MISO bytes
 * (via the shared pi3_rx / Module.ccall path).
 *
 * Register map (BCM2835 SPI0):
 *   +0x00 CS     control/status (TA, CLEAR, DONE, TXD, RXD, etc.)
 *   +0x04 FIFO   TX/RX data (read pops RX, write pushes TX)
 *   +0x08 CLK    clock divider
 *   +0x0C DLEN   data length
 *   +0x10 LTOH   lo-threshold
 *   +0x14 DC     DMA control
 *
 * C<->browser transport: emscripten postMessage (worker -> main thread)
 * and a shared pi3_rx function (main thread -> worker).
 */

#ifndef SPI_BRIDGE_H
#define SPI_BRIDGE_H

#include "hw/sysbus.h"
#include "qom/object.h"

#define TYPE_SPI_BRIDGE "spi-bridge"
#define SPI_BRIDGE_SIZE 0x1000
#define SPI_FIFO_DEPTH 64

struct SPIBridgeState {
    SysBusDevice parent_obj;
    MemoryRegion mmio;
    uint32_t cs;            /* CS register */
    uint32_t clk;           /* CLK register */
    uint32_t dlen;          /* DLEN register */
    uint8_t tx_fifo[SPI_FIFO_DEPTH];
    int tx_len;
    uint8_t rx_fifo[SPI_FIFO_DEPTH];
    int rx_len;
    int rx_pos;             /* read pointer into rx_fifo */
    bool guard;             /* suppress tx hook during host writes */
};
typedef struct SPIBridgeState SPIBridgeState;

OBJECT_DECLARE_SIMPLE_TYPE(SPIBridgeState, SPI_BRIDGE)

#endif /* SPI_BRIDGE_H */
