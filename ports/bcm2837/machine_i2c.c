// BCM2837 machine.I2C self-contained high-level API.
//
// The shared extmod protocol layer misbehaves on this minimal config
// (methods resolving to wrong handlers), so this file implements the
// Pico-compatible surface directly on top of the proven BSC transfer
// engine: scan/init/deinit/readfrom/readfrom_into/writeto/readfrom_mem/
// writeto_mem. Semantics match upstream: writeto returns ACK count,
// readfrom_mem does write-then-read, scan probes 0x08..0x77.

#include "py/runtime.h"
#include "py/mphal.h"
#include "py/mperrno.h"

#define BSC_C(base)     (*(volatile unsigned *)((uintptr_t)(base) + 0x00))
#define BSC_S(base)     (*(volatile unsigned *)((uintptr_t)(base) + 0x04))
#define BSC_DLEN(base)  (*(volatile unsigned *)((uintptr_t)(base) + 0x08))
#define BSC_A(base)     (*(volatile unsigned *)((uintptr_t)(base) + 0x0C))
#define BSC_FIFO(base)  (*(volatile unsigned *)((uintptr_t)(base) + 0x10))
#define TMR_CLO (*(volatile unsigned *)(0x3F003000UL + 0x04))

#define BSC_I2CEN (1u << 15)
#define BSC_ST    (1u << 7)
#define BSC_CLEAR (1u << 4)
#define BSC_READ  (1u << 0)
#define BSC_DONE  (1u << 7)

#define BSC_CHUNK (4)
#define DEFAULT_TIMEOUT_US (50000)

typedef struct _machine_i2c_obj_t {
    mp_obj_base_t base;
    uintptr_t regs;
    unsigned freq;
    unsigned timeout;
} machine_i2c_obj_t;

extern const mp_obj_type_t machine_i2c_type;

static unsigned bsc_base(int id) {
    if (id == 0) {
        return 0x3F205000u; // BSC0
    } else if (id == 1) {
        return 0x3F804000u; // BSC1
    }
    mp_raise_ValueError(MP_ERROR_TEXT("I2C id 0..1"));
    return 0;
}

static int bsc_wait_done(uintptr_t base, unsigned timeout_us) {
    unsigned start = TMR_CLO;
    while (!(BSC_S(base) & BSC_DONE)) {
        if ((TMR_CLO - start) >= timeout_us) {
            return -MP_ETIMEDOUT;
        }
    }
    return 0;
}

// One complete BSC transaction of n <= 4 bytes. Returns 0 or -errno.
static int bsc_xfer(uintptr_t base, unsigned addr, uint8_t *buf, unsigned n, int read, unsigned timeout_us) {
    unsigned i, w;
    unsigned start;
    int ret;
    BSC_C(base) = BSC_CLEAR; // drop ST for a clean edge, clear DONE
    // Drop-sync: S.DONE is slice-boundary-visible, so a DONE left over from
    // the previous transfer is still showing. Wait for it to clear before
    // issuing ST, or the completion poll below would see stale DONE and
    // return before this transfer runs (same stale-window race the
    // bare-metal i2c guest handles with a pre-wait loop).
    start = TMR_CLO;
    while (BSC_S(base) & BSC_DONE) {
        if ((TMR_CLO - start) >= timeout_us) {
            return -MP_ETIMEDOUT;
        }
    }
    BSC_DLEN(base) = n;
    BSC_A(base) = addr & 0x7F;
    if (!read) {
        w = 0;
        for (i = 0; i < n; i++) {
            w |= (unsigned)buf[i] << (8 * i);
        }
        BSC_FIFO(base) = w;
        BSC_C(base) = BSC_I2CEN | BSC_ST;
    } else {
        BSC_C(base) = BSC_I2CEN | BSC_ST | BSC_READ;
    }
    ret = bsc_wait_done(base, timeout_us);
    if (ret != 0) {
        return ret;
    }
    if (read) {
        w = BSC_FIFO(base);
        for (i = 0; i < n; i++) {
            buf[i] = (w >> (8 * i)) & 0xFF;
        }
    }
    BSC_C(base) = BSC_CLEAR;
    return 0;
}

// Chunked transfer; returns bytes moved or raises OSError.
static size_t i2c_xfer(machine_i2c_obj_t *self, unsigned addr, uint8_t *buf, size_t len, int read) {
    size_t done = 0;
    if (addr > 0x7F) {
        mp_raise_ValueError(MP_ERROR_TEXT("addr 0..0x7F"));
    }
    while (done < len) {
        size_t n = len - done;
        if (n > BSC_CHUNK) {
            n = BSC_CHUNK;
        }
        int ret = bsc_xfer(self->regs, addr, buf + done, n, read, self->timeout);
        if (ret != 0) {
            mp_raise_OSError(-ret);
        }
        done += n;
    }
    return done;
}

static mp_obj_t i2c_make_new(const mp_obj_type_t *type, size_t n_args, size_t n_kw, const mp_obj_t *all_args) {
    enum { ARG_id, ARG_scl, ARG_sda, ARG_freq, ARG_timeout };
    static const mp_arg_t allowed[] = {
        { MP_QSTR_id,      MP_ARG_REQUIRED | MP_ARG_INT, {.u_int = 1} },
        { MP_QSTR_scl,     MP_ARG_OBJ,                   {.u_rom_obj = MP_ROM_NONE} },
        { MP_QSTR_sda,     MP_ARG_OBJ,                   {.u_rom_obj = MP_ROM_NONE} },
        { MP_QSTR_freq,    MP_ARG_INT,                   {.u_int = 400000} },
        { MP_QSTR_timeout, MP_ARG_KW_ONLY | MP_ARG_INT,  {.u_int = DEFAULT_TIMEOUT_US} },
    };
    mp_arg_val_t args[MP_ARRAY_SIZE(allowed)];
    mp_arg_parse_all_kw_array(n_args, n_kw, all_args, MP_ARRAY_SIZE(allowed), allowed, args);
    machine_i2c_obj_t *self = mp_obj_malloc(machine_i2c_obj_t, &machine_i2c_type);
    self->regs = bsc_base(args[ARG_id].u_int);
    self->freq = (unsigned)args[ARG_freq].u_int;
    self->timeout = (unsigned)args[ARG_timeout].u_int;
    return MP_OBJ_FROM_PTR(self);
}

static void i2c_print(const mp_print_t *print, mp_obj_t self_in, mp_print_kind_t kind) {
    machine_i2c_obj_t *self = MP_OBJ_TO_PTR(self_in);
    unsigned id = self->regs == 0x3F205000u ? 0 : 1;
    mp_printf(print, "I2C(%u, freq=%u)", id, self->freq);
}

static mp_obj_t i2c_scan(mp_obj_t self_in) {
    machine_i2c_obj_t *self = MP_OBJ_TO_PTR(self_in);
    mp_obj_t list = mp_obj_new_list(0, NULL);
    // 7-bit addresses 0b0000xxx and 0b1111xxx are reserved.
    for (int addr = 0x08; addr < 0x78; addr++) {
        uint8_t dummy = 0;
        if (bsc_xfer(self->regs, (unsigned)addr, &dummy, 0, 0, self->timeout) == 0) {
            // Zero-length probe: DONE means something answered. The register-
            // select slave answers every address; real buses NACK empties.
            mp_obj_list_append(list, MP_OBJ_NEW_SMALL_INT(addr));
        }
    }
    return list;
}
static MP_DEFINE_CONST_FUN_OBJ_1(i2c_scan_obj, i2c_scan);

static mp_obj_t i2c_readfrom(size_t n_args, const mp_obj_t *args) {
    // readfrom(addr, nbytes, stop=True)
    machine_i2c_obj_t *self = MP_OBJ_TO_PTR(args[0]);
    unsigned addr = (unsigned)mp_obj_get_int(args[1]);
    size_t len = (size_t)mp_obj_get_int(args[2]);
    vstr_t vstr;
    vstr_init_len(&vstr, len);
    i2c_xfer(self, addr, (uint8_t *)vstr.buf, len, 1);
    return mp_obj_new_bytes_from_vstr(&vstr);
}
static MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN(i2c_readfrom_obj, 3, 4, i2c_readfrom);

static mp_obj_t i2c_writeto(size_t n_args, const mp_obj_t *args) {
    // writeto(addr, buf, stop=True) -> ACK count
    machine_i2c_obj_t *self = MP_OBJ_TO_PTR(args[0]);
    unsigned addr = (unsigned)mp_obj_get_int(args[1]);
    mp_buffer_info_t bufinfo;
    mp_get_buffer_raise(args[2], &bufinfo, MP_BUFFER_READ);
    size_t n = i2c_xfer(self, addr, bufinfo.buf, bufinfo.len, 0);
    return MP_OBJ_NEW_SMALL_INT(n);
}
static MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN(i2c_writeto_obj, 3, 4, i2c_writeto);

static mp_obj_t i2c_readfrom_mem(size_t n_args, const mp_obj_t *args) {
    // readfrom_mem(addr, memaddr, nbytes, addrsize=8)
    machine_i2c_obj_t *self = MP_OBJ_TO_PTR(args[0]);
    unsigned addr = (unsigned)mp_obj_get_int(args[1]);
    unsigned memaddr = (unsigned)mp_obj_get_int(args[2]);
    size_t len = (size_t)mp_obj_get_int(args[3]);
    uint8_t reg = memaddr & 0xFF;
    i2c_xfer(self, addr, &reg, 1, 0);
    vstr_t vstr;
    vstr_init_len(&vstr, len);
    i2c_xfer(self, addr, (uint8_t *)vstr.buf, len, 1);
    return mp_obj_new_bytes_from_vstr(&vstr);
}
static MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN(i2c_readfrom_mem_obj, 4, 5, i2c_readfrom_mem);

static mp_obj_t i2c_writeto_mem(size_t n_args, const mp_obj_t *args) {
    // writeto_mem(addr, memaddr, buf, addrsize=8)
    machine_i2c_obj_t *self = MP_OBJ_TO_PTR(args[0]);
    unsigned addr = (unsigned)mp_obj_get_int(args[1]);
    unsigned memaddr = (unsigned)mp_obj_get_int(args[2]);
    mp_buffer_info_t bufinfo;
    mp_get_buffer_raise(args[3], &bufinfo, MP_BUFFER_READ);
    uint8_t reg = memaddr & 0xFF;
    i2c_xfer(self, addr, &reg, 1, 0);
    size_t n = i2c_xfer(self, addr, bufinfo.buf, bufinfo.len, 0);
    return MP_OBJ_NEW_SMALL_INT(1 + n);
}
static MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN(i2c_writeto_mem_obj, 4, 5, i2c_writeto_mem);

static const mp_rom_map_elem_t i2c_locals_dict_table[] = {
    { MP_ROM_QSTR(MP_QSTR_scan),          MP_ROM_PTR(&i2c_scan_obj) },
    { MP_ROM_QSTR(MP_QSTR_readfrom),      MP_ROM_PTR(&i2c_readfrom_obj) },
    { MP_ROM_QSTR(MP_QSTR_writeto),       MP_ROM_PTR(&i2c_writeto_obj) },
    { MP_ROM_QSTR(MP_QSTR_readfrom_mem),  MP_ROM_PTR(&i2c_readfrom_mem_obj) },
    { MP_ROM_QSTR(MP_QSTR_writeto_mem),   MP_ROM_PTR(&i2c_writeto_mem_obj) },
};
static MP_DEFINE_CONST_DICT(i2c_locals_dict, i2c_locals_dict_table);

MP_DEFINE_CONST_OBJ_TYPE(
    machine_i2c_type,
    MP_QSTR_I2C,
    MP_TYPE_FLAG_NONE,
    make_new, i2c_make_new,
    print, i2c_print,
    locals_dict, &i2c_locals_dict
    );
