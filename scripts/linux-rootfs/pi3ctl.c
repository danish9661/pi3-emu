/*
 * pi3-ctl: browser <-> BCM2835 GPIO bridge device.
 * See hw/misc/pi3ctl.h for the wiring contract.
 */

#include "qemu/osdep.h"
#include "qemu/module.h"
#include "qemu/log.h"
#include "qapi/error.h"
#include "hw/sysbus.h"
#include "hw/qdev-properties.h"
#include "hw/qdev-properties-system.h"
#include "hw/irq.h"
#include "hw/misc/pi3ctl.h"
#include <emscripten.h>

static PI3CtlState *current_pi3;

void pi3_rx(const char *s);

/* C -> browser: post a line of text to the main thread (worker context). */
EM_JS(void, pi3_js_tx, (int len, const char *data), {
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

/* browser -> device: "I <line> <0|1>" sets a browser-driven input level. */
EMSCRIPTEN_KEEPALIVE
void pi3_rx(const char *s)
{
    unsigned line = 0, val = 0;
    if (sscanf(s, "I %u %u", &line, &val) == 2 && line < PI3_NLINES) {
        if (!current_pi3) {
            return;
        }
        if (val) {
            current_pi3->browser_levels |= (1ULL << line);
        } else {
            current_pi3->browser_levels &= ~(1ULL << line);
        }
        qemu_set_irq(current_pi3->browser_in[line], val ? 1 : 0);
    }
}

/* bcm2835_gpio.out changed (guest wrote a GPIO output) -> tell the browser. */
static void pi3_guest_out_set(void *opaque, int line, int level)
{
    char buf[32];
    int n = snprintf(buf, sizeof(buf), "S %d %d\n", line, level ? 1 : 0);
    pi3_js_tx(n, buf);
}

static void pi3ctl_realize(DeviceState *dev, Error **errp)
{
    PI3CtlState *s = PI3_CTL(dev);
    current_pi3 = s;
    qdev_init_gpio_in(dev, pi3_guest_out_set, PI3_NLINES);
    qdev_init_gpio_out(dev, s->browser_in, PI3_NLINES);
}

static void pi3ctl_class_init(ObjectClass *klass, void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    dc->realize = pi3ctl_realize;
}

static const TypeInfo pi3ctl_info = {
    .name = TYPE_PI3_CTL,
    .parent = TYPE_DEVICE,
    .instance_size = sizeof(PI3CtlState),
    .class_init = pi3ctl_class_init,
};

static void pi3ctl_register_types(void)
{
    type_register_static(&pi3ctl_info);
}

type_init(pi3ctl_register_types)
