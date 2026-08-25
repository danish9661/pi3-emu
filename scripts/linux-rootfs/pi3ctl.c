/*
 * pi3-ctl — a tiny sysbus device that bridges the emulated GPIO to the
 * browser, for the pi3-emu Linux environment.
 *
 * This is the "true" N4 bridge: instead of tunneling GPIO over the serial
 * console (the harness-level bridge in public/linux/index.html), the guest
 * talks to this device's MMIO registers and the device forwards line state
 * to a qemu chardev (`chr`). The browser side is a `web`/postMessage
 * chardev backend (see AGENTS.md N4) that relays `chr` traffic to JS.
 *
 * Build/integration (apply on top of ktock/qemu-wasm, then rebuild with the
 * pthread/MTTCG emscripten flags — this checkout cannot build that, so this
 * file is source-only / unverified in CI):
 *   1. drop this file at hw/misc/pi3ctl.c
 *   2. add 'pi3ctl.c' to hw/misc/meson.build
 *   3. in hw/arm/raspi.c (raspi3ap machine) object_initialize_child a
 *      TYPE_PI3_CTL and sysbus_mmio_map it at an unused 0x4000X000 region,
 *      and set its "chr" property to a chardev created from the qemu args
 *      (-chardev web,id=pi3 -device pi3-ctl,chr=pi3).
 *
 * MMIO:
 *   0x00  WO  set line  (val: bits[5:0]=line, bit[8]=1)
 *   0x08  WO  clear line(bits[5:0]=line)
 *   0x00  RO  current output levels (uint64)
 *   0x08  RO  current input levels (uint64, fed from chardev "I <l> <v>")
 *   chardev TX (device -> browser): "S <line> <0|1>\n" on every output change
 *   chardev RX (browser -> device): "I <line> <0|1>\n" updates an input line
 */
#include "qemu/osdep.h"
#include "qemu/module.h"
#include "qemu/log.h"
#include "qapi/error.h"
#include "hw/sysbus.h"
#include "hw/qdev-properties.h"
#include "chardev/char-fe.h"

#define TYPE_PI3_CTL "pi3-ctl"
OBJECT_DECLARE_SIMPLE_TYPE(PI3CtlState, PI3_CTL)

#define PI3_CTL_NLINES 64

struct PI3CtlState {
    SysBusDevice parent_obj;
    MemoryRegion iomem;
    CharBackend chr;
    uint64_t out_levels;   /* lines driven by the guest */
    uint64_t in_levels;    /* lines driven by the browser (via chardev) */
};

static void pi3ctl_emit(PI3CtlState *s, unsigned line, unsigned v)
{
    char buf[32];
    int n = snprintf(buf, sizeof(buf), "S %u %u\n", line, v);
    if (n > 0) qemu_chr_fe_write_all(&s->chr, (uint8_t *)buf, n);
}

static void pi3ctl_chr_event(void *opaque, QEMUChrEvent ev) { /* no-op */ }

static int pi3ctl_chr_can_read(void *opaque) { return 1; }

static void pi3ctl_chr_read(void *opaque, const uint8_t *data, int size)
{
    PI3CtlState *s = opaque;
    /* Expect lines like "I <line> <0|1>\n". Minimal parser. */
    static char line[64];
    static int len = 0;
    for (int i = 0; i < size; i++) {
        char c = (char)data[i];
        if (c == '\n' || c == '\r') {
            line[len] = 0;
            unsigned line_no = 0, val = 0;
            if (sscanf(line, "I %u %u", &line_no, &val) == 2
                    && line_no < PI3_CTL_NLINES) {
                if (val) s->in_levels |= (1ULL << line_no);
                else     s->in_levels &= ~(1ULL << line_no);
            }
            len = 0;
        } else if (len < (int)sizeof(line) - 1) {
            line[len++] = c;
        }
    }
}

static uint64_t pi3ctl_read(void *opaque, hwaddr offset, unsigned size)
{
    PI3CtlState *s = opaque;
    switch (offset) {
    case 0x00: return s->out_levels;
    case 0x08: return s->in_levels;
    default:   return 0;
    }
}

static void pi3ctl_write(void *opaque, hwaddr offset, uint64_t value, unsigned size)
{
    PI3CtlState *s = opaque;
    unsigned line = value & 0x3f;
    if (line >= PI3_CTL_NLINES) return;
    if (offset == 0x00) {
        s->out_levels |= (1ULL << line);
        pi3ctl_emit(s, line, 1);
    } else if (offset == 0x08) {
        s->out_levels &= ~(1ULL << line);
        pi3ctl_emit(s, line, 0);
    }
}

static const MemoryRegionOps pi3ctl_ops = {
    .read = pi3ctl_read,
    .write = pi3ctl_write,
    .endianness = DEVICE_NATIVE_ENDIAN,
};

static void pi3ctl_init(Object *obj)
{
    PI3CtlState *s = PI3_CTL(obj);
    SysBusDevice *sbd = SYS_BUS_DEVICE(obj);
    memory_region_init_io(&s->iomem, obj, &pi3ctl_ops, s, "pi3-ctl", 0x1000);
    sysbus_init_mmio(sbd, &s->iomem);
}

static void pi3ctl_realize(DeviceState *dev, Error **errp)
{
    PI3CtlState *s = PI3_CTL(dev);
    if (!qemu_chr_fe_backend_connected(&s->chr)) {
        /* chardev is optional; without it the device is write-only local. */
        qemu_chr_fe_set_handlers(&s->chr, pi3ctl_chr_can_read,
            pi3ctl_chr_read, pi3ctl_chr_event, NULL, s, NULL, true);
    } else {
        qemu_chr_fe_set_handlers(&s->chr, pi3ctl_chr_can_read,
            pi3ctl_chr_read, pi3ctl_chr_event, NULL, s, NULL, true);
    }
}

static Property pi3ctl_props[] = {
    DEFINE_PROP_CHR("chr", PI3CtlState, chr),
    DEFINE_PROP_END_OF_LIST(),
};

static void pi3ctl_class_init(ObjectClass *klass, void *data)
{
    DeviceClass *dc = DEVICE_CLASS(klass);
    dc->realize = pi3ctl_realize;
    device_class_set_props(dc, pi3ctl_props);
}

static const TypeInfo pi3ctl_info = {
    .name = TYPE_PI3_CTL,
    .parent = TYPE_SYS_BUS_DEVICE,
    .instance_size = sizeof(PI3CtlState),
    .instance_init = pi3ctl_init,
    .class_init = pi3ctl_class_init,
};

static void pi3ctl_register_types(void)
{
    type_register_static(&pi3ctl_info);
}

type_init(pi3ctl_register_types)
