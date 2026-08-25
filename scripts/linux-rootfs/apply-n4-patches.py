#!/usr/bin/env python3
"""Apply the N4 (real GPIO bridge) patches to the qemu-wasm clone.

Run from the qemu-wasm source root (the cloned repo), after the clone and
before `emconfigure`/`configure`. Idempotent-ish: it asserts each anchor
is present exactly once and fails loudly if a patch has already been
applied or the tree has drifted.
"""
import sys
import os

ROOT = os.getcwd()

FILES = {
    "gpio_h": os.path.join(ROOT, "include/hw/gpio/bcm2835_gpio.h"),
    "gpio_c": os.path.join(ROOT, "hw/gpio/bcm2835_gpio.c"),
    "raspi_c": os.path.join(ROOT, "hw/arm/raspi.c"),
}


def patch(path, old, new):
    with open(path, "r") as f:
        src = f.read()
    if old not in src:
        raise SystemExit(f"ANCHOR NOT FOUND in {path}:\n{old}")
    if src.count(old) != 1:
        raise SystemExit(f"ANCHOR AMBIGUOUS ({src.count(old)}) in {path}")
    src = src.replace(old, new)
    with open(path, "w") as f:
        f.write(src)
    print(f"patched {path}")


# 1) bcm2835_gpio.h: add input line state + input qemu_irq array.
patch(
    FILES["gpio_h"],
    """    uint8_t fsel[54];
    uint32_t lev0, lev1;
    uint8_t sd_fsel;
    qemu_irq out[54];
};""",
    """    uint8_t fsel[54];
    uint32_t lev0, lev1;
    uint32_t in_lev0, in_lev1;
    uint8_t sd_fsel;
    qemu_irq out[54];
    qemu_irq in[54];
};""",
)

# 2) bcm2835_gpio.c: GPLEV reflects browser-driven input levels.
patch(
    FILES["gpio_c"],
    """    case GPLEV0:
        return s->lev0;
    case GPLEV1:
        return s->lev1;""",
    """    case GPLEV0:
        return s->lev0 | s->in_lev0;
    case GPLEV1:
        return s->lev1 | s->in_lev1;""",
)

# 3) bcm2835_gpio.c: reset the input level state too.
patch(
    FILES["gpio_c"],
    """    s->lev0 = 0;
    s->lev1 = 0;
}""",
    """    s->lev0 = 0;
    s->lev1 = 0;
    s->in_lev0 = 0;
    s->in_lev1 = 0;
}""",
)

# 4) bcm2835_gpio.c: register the input GPIO lines.
patch(
    FILES["gpio_c"],
    """    qdev_init_gpio_out(dev, s->out, 54);
}""",
    """    qdev_init_gpio_out(dev, s->out, 54);
    qdev_init_gpio_in(dev, bcm2835_gpio_in_set, 54);
}""",
)

# 5) bcm2835_gpio.c: the input handler (inserted before the reset fn).
patch(
    FILES["gpio_c"],
    "static void bcm2835_gpio_reset(DeviceState *dev)",
    """static void bcm2835_gpio_in_set(void *opaque, int line, int level)
{
    BCM2835GpioState *s = BCM2835_GPIO(opaque);
    if (line < 0 || line >= 54) {
        return;
    }
    if (line < 32) {
        if (level) {
            s->in_lev0 |= (1u << line);
        } else {
            s->in_lev0 &= ~(1u << line);
        }
    } else {
        if (level) {
            s->in_lev1 |= (1u << (line - 32));
        } else {
            s->in_lev1 &= ~(1u << (line - 32));
        }
    }
}

static void bcm2835_gpio_reset(DeviceState *dev)""",
)

# 6) raspi.c: include the pi3-ctl header.
patch(
    FILES["raspi_c"],
    '#include "hw/arm/bcm2836.h"',
    '#include "hw/arm/bcm2836.h"\n#include "hw/misc/pi3ctl.h"',
)

# 7) raspi.c: instantiate + wire pi3-ctl right after the SoC is realized.
patch(
    FILES["raspi_c"],
    "    qdev_realize(DEVICE(&s->soc), NULL, &error_fatal);\n",
    """    qdev_realize(DEVICE(&s->soc), NULL, &error_fatal);

    /* N4: pi3-ctl browser<->GPIO bridge. Wired directly to the real
     * bcm2835_gpio input/output lines (no MMIO, no address collision). */
    {
        DeviceState *gpio = DEVICE(&s->soc.peripherals.gpio);
        DeviceState *pi3 = qdev_new(TYPE_PI3_CTL);
        object_property_add_child(OBJECT(machine), "pi3-ctl", OBJECT(pi3));
        qdev_realize(pi3, NULL, &error_fatal);
        for (int i = 0; i < PI3_NLINES; i++) {
            qdev_connect_gpio_out(gpio, i, qdev_get_gpio_in(pi3, i));
            qdev_connect_gpio_out(pi3, i, qdev_get_gpio_in(gpio, i));
        }
    }
""",
)

print("N4 patches applied OK")
