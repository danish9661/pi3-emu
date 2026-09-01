/*
 * i2c-bridge: browser <-> BCM2835 I2C (BSC) bridge device.
 * See hw/misc/i2c-bridge.h for the register map and wiring contract.
 *
 * Emulates the BCM2835 BSC master registers at 0x3F804000.  Guest I2C
 * transfers are forwarded to the browser as:
 *   "I2C_TX <addr> <reg> <hex bytes>\n"  (write transfer)
 *   "I2C_RD  <addr> <reg> <dlen>\n"      (read transfer)
 * The browser can respond to reads with:
 *   "I2C_RX <hex bytes>\n"               (response data)
 *
 * Register map (BCM2835 BSC):
 *   +0x00 C     control (I2CEN, ST, CLEAR, READ)
 *   +0x04 S     status (DONE)
 *   +0x08 DLEN  data length
 *   +0x0C A     slave address
 *   +0x10 FIFO  data (read pops, write pushes)
 *   +0x14 DIV   clock divider
 *   +0x18 DEL   SDA delay
 *   +0x1C CLKT  clock stretch timeout
 *
 * C<->browser transport: emscripten postMessage (worker -> main thread)
 * and a shared pi3_rx function (main thread -> worker).
 */

#ifndef I2C_BRIDGE_H
#define I2C_BRIDGE_H

#include "hw/sysbus.h"
#include "qom/object.h"

#define TYPE_I2C_BRIDGE "i2c-bridge"
#define I2C_BRIDGE_SIZE 0x1000
#define I2C_FIFO_DEPTH 16

struct I2CBridgeState {
    SysBusDevice parent_obj;
    MemoryRegion mmio;
    uint32_t c;             /* C register */
    uint32_t s;             /* S register */
    uint32_t dlen;          /* DLEN register */
    uint32_t addr;          /* A register (7-bit slave address) */
    uint32_t div;           /* DIV register */
    uint8_t fifo[I2C_FIFO_DEPTH];
    int fifo_len;
    int fifo_pos;
    uint8_t resp[I2C_FIFO_DEPTH];
    int resp_len;
    int resp_pos;
    uint8_t reg;            /* register address (latched from first write byte) */
};
typedef struct I2CBridgeState I2CBridgeState;

OBJECT_DECLARE_SIMPLE_TYPE(I2CBridgeState, I2C_BRIDGE)

#endif /* I2C_BRIDGE_H */
