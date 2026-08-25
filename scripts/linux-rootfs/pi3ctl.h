/*
 * pi3-ctl: browser <-> BCM2835 GPIO bridge device.
 *
 * This is a pure bridge. It has no MMIO of its own. Instead it is wired
 * directly to the real bcm2835_gpio device's output and input GPIO lines
 * by the board code (hw/arm/raspi.c):
 *
 *   bcm2835_gpio.out[line]  ->  pi3-ctl input  (guest writes a GPIO pin)
 *       pi3-ctl forwards this to the browser as "S <line> <0|1>\n".
 *
 *   pi3-ctl output[line]    ->  bcm2835_gpio.in[line]
 *       the browser sends "I <line> <0|1>\n" which makes the guest see
 *       the level on that pin via GPLEV (a real, emulated input).
 *
 * C<->browser transport is emscripten postMessage (worker -> main thread)
 * and an exported C function pi3_rx (main thread -> worker, proxied).
 */

#ifndef PI3CTL_H
#define PI3CTL_H

#include "hw/sysbus.h"
#include "qom/object.h"

#define TYPE_PI3_CTL "pi3-ctl"
#define PI3_NLINES 54

struct PI3CtlState {
    DeviceState parent_obj;
    qemu_irq guest_out[PI3_NLINES];   /* input: driven by bcm2835_gpio.out */
    qemu_irq browser_in[PI3_NLINES];  /* output: drive bcm2835_gpio.in */
    uint64_t browser_levels;
};
typedef struct PI3CtlState PI3CtlState;

OBJECT_DECLARE_SIMPLE_TYPE(PI3CtlState, PI3_CTL)

#endif /* PI3CTL_H */
