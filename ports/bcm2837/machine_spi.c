// BCM2837 machine.SPI self-contained high-level API (see machine_i2c.c
// header for why extmod's shared dicts are not used): read/readinto/write/
// write_readinto on SPI0. Transfers cap at 4 bytes (the model's FIFO
// window); longer ones raise ValueError. Only id 0 exists here.

#include "py/runtime.h"
#include "py/mphal.h"
#include "py/mperrno.h"
#include <stdint.h>

#define SPI0_BASE (0x3F204000u)
#define SPI_CS(base)   (*(volatile unsigned *)((uintptr_t)(base) + 0x00))
#define SPI_FIFO(base) (*(volatile unsigned *)((uintptr_t)(base) + 0x04))
#define TMR_CLO (*(volatile unsigned *)(0x3F003000UL + 0x04))

#define SPI_TA    (1u << 7)
#define SPI_CLEAR (0x3u << 4)
#define SPI_DONE  (1u << 16)

#define SPI_MAX_XFER (4)
#define DEFAULT_TIMEOUT_US (50000)

typedef struct _machine_spi_obj_t {
    mp_obj_base_t base;
    uintptr_t regs;
    unsigned baudrate;
} machine_spi_obj_t;

extern const mp_obj_type_t machine_spi_type;

static void spi_xfer(machine_spi_obj_t *self, size_t len, const uint8_t *src, uint8_t *dest) {
    uintptr_t base = self->regs;
    unsigned start = TMR_CLO;
    size_t i;
    unsigned w;
    if (len == 0 || len > SPI_MAX_XFER) {
        mp_raise_ValueError(MP_ERROR_TEXT("transfer 1..4 bytes"));
    }
    SPI_CS(base) = SPI_CLEAR; // reset session, drop TA for a clean edge
    // Drop-sync (see machine_i2c.c): the DONE cell is slice-boundary-
    // refreshed, so a leftover DONE would complete the poll below instantly.
    start = TMR_CLO;
    while (SPI_CS(base) & SPI_DONE) {
        if ((TMR_CLO - start) >= DEFAULT_TIMEOUT_US) {
            mp_raise_OSError(MP_ETIMEDOUT);
        }
    }
    w = 0;
    for (i = 0; i < len; i++) {
        w |= (unsigned)src[i] << (8 * i);
    }
    SPI_FIFO(base) = w;
    SPI_CS(base) = SPI_TA | 1u; // TA rise starts the transaction (CS0)
    while (!(SPI_CS(base) & SPI_DONE)) {
        if ((TMR_CLO - start) >= DEFAULT_TIMEOUT_US) {
            SPI_CS(base) = SPI_CLEAR;
            mp_raise_OSError(MP_ETIMEDOUT);
        }
    }
    w = SPI_FIFO(base);
    for (i = 0; i < len; i++) {
        dest[i] = (w >> (8 * i)) & 0xFF;
    }
    SPI_CS(base) = SPI_CLEAR;
}

static mp_obj_t spi_make_new(const mp_obj_type_t *type, size_t n_args, size_t n_kw, const mp_obj_t *all_args) {
    enum { ARG_id, ARG_baudrate, ARG_polarity, ARG_phase, ARG_bits, ARG_firstbit, ARG_sck, ARG_mosi, ARG_miso };
    static const mp_arg_t allowed[] = {
        { MP_QSTR_id,       MP_ARG_REQUIRED | MP_ARG_INT, {.u_int = 0} },
        { MP_QSTR_baudrate, MP_ARG_INT,                   {.u_int = 500000} },
        { MP_QSTR_polarity, MP_ARG_INT,                   {.u_int = 0} },
        { MP_QSTR_phase,    MP_ARG_INT,                   {.u_int = 0} },
        { MP_QSTR_bits,     MP_ARG_INT,                   {.u_int = 8} },
        { MP_QSTR_firstbit, MP_ARG_KW_ONLY | MP_ARG_INT,  {.u_int = 0} },
        { MP_QSTR_sck,      MP_ARG_OBJ,                   {.u_rom_obj = MP_ROM_NONE} },
        { MP_QSTR_mosi,     MP_ARG_OBJ,                   {.u_rom_obj = MP_ROM_NONE} },
        { MP_QSTR_miso,     MP_ARG_OBJ,                   {.u_rom_obj = MP_ROM_NONE} },
    };
    mp_arg_val_t args[MP_ARRAY_SIZE(allowed)];
    mp_arg_parse_all_kw_array(n_args, n_kw, all_args, MP_ARRAY_SIZE(allowed), allowed, args);
    if (args[ARG_id].u_int != 0) {
        mp_raise_ValueError(MP_ERROR_TEXT("SPI id 0 only"));
    }
    if (args[ARG_bits].u_int != 8) {
        mp_raise_ValueError(MP_ERROR_TEXT("8 bits only"));
    }
    machine_spi_obj_t *self = mp_obj_malloc(machine_spi_obj_t, &machine_spi_type);
    self->regs = SPI0_BASE;
    self->baudrate = (unsigned)args[ARG_baudrate].u_int;
    return MP_OBJ_FROM_PTR(self);
}

static void spi_print(const mp_print_t *print, mp_obj_t self_in, mp_print_kind_t kind) {
    machine_spi_obj_t *self = MP_OBJ_TO_PTR(self_in);
    mp_printf(print, "SPI(0, baudrate=%u)", self->baudrate);
}

static mp_obj_t spi_read(size_t n_args, const mp_obj_t *args) {
    machine_spi_obj_t *self = MP_OBJ_TO_PTR(args[0]);
    size_t len = (size_t)mp_obj_get_int(args[1]);
    uint8_t fill = n_args > 2 ? (uint8_t)mp_obj_get_int(args[2]) : 0;
    vstr_t vstr;
    vstr_init_len(&vstr, len);
    for (size_t i = 0; i < len; i++) {
        vstr.buf[i] = (char)fill;
    }
    spi_xfer(self, len, (uint8_t *)vstr.buf, (uint8_t *)vstr.buf);
    return mp_obj_new_bytes_from_vstr(&vstr);
}
static MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN(spi_read_obj, 2, 3, spi_read);

static mp_obj_t spi_readinto(size_t n_args, const mp_obj_t *args) {
    machine_spi_obj_t *self = MP_OBJ_TO_PTR(args[0]);
    mp_buffer_info_t bufinfo;
    mp_get_buffer_raise(args[1], &bufinfo, MP_BUFFER_WRITE);
    uint8_t fill = n_args > 2 ? (uint8_t)mp_obj_get_int(args[2]) : 0;
    for (size_t i = 0; i < bufinfo.len; i++) {
        ((uint8_t *)bufinfo.buf)[i] = fill;
    }
    spi_xfer(self, bufinfo.len, bufinfo.buf, bufinfo.buf);
    return mp_const_none;
}
static MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN(spi_readinto_obj, 2, 3, spi_readinto);

static mp_obj_t spi_write(size_t n_args, const mp_obj_t *args) {
    machine_spi_obj_t *self = MP_OBJ_TO_PTR(args[0]);
    mp_buffer_info_t bufinfo;
    mp_get_buffer_raise(args[1], &bufinfo, MP_BUFFER_READ);
    // Full-duplex discard: response bytes clock in but are dropped.
    uint8_t dummy[SPI_MAX_XFER];
    if (bufinfo.len > SPI_MAX_XFER) {
        mp_raise_ValueError(MP_ERROR_TEXT("transfer 1..4 bytes"));
    }
    spi_xfer(self, bufinfo.len, bufinfo.buf, dummy);
    return mp_const_none;
}
static MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN(spi_write_obj, 2, 3, spi_write);

static mp_obj_t spi_write_readinto(mp_obj_t self_in, mp_obj_t wr_buf, mp_obj_t rd_buf) {
    machine_spi_obj_t *self = MP_OBJ_TO_PTR(self_in);
    mp_buffer_info_t src, dest;
    mp_get_buffer_raise(wr_buf, &src, MP_BUFFER_READ);
    mp_get_buffer_raise(rd_buf, &dest, MP_BUFFER_WRITE);
    if (src.len != dest.len) {
        mp_raise_ValueError(MP_ERROR_TEXT("buffers must be the same length"));
    }
    spi_xfer(self, src.len, src.buf, dest.buf);
    return mp_const_none;
}
static MP_DEFINE_CONST_FUN_OBJ_3(spi_write_readinto_obj, spi_write_readinto);

static const mp_rom_map_elem_t spi_locals_dict_table[] = {
    { MP_ROM_QSTR(MP_QSTR_read), MP_ROM_PTR(&spi_read_obj) },
    { MP_ROM_QSTR(MP_QSTR_readinto), MP_ROM_PTR(&spi_readinto_obj) },
    { MP_ROM_QSTR(MP_QSTR_write), MP_ROM_PTR(&spi_write_obj) },
    { MP_ROM_QSTR(MP_QSTR_write_readinto), MP_ROM_PTR(&spi_write_readinto_obj) },
};
static MP_DEFINE_CONST_DICT(spi_locals_dict, spi_locals_dict_table);

MP_DEFINE_CONST_OBJ_TYPE(
    machine_spi_type,
    MP_QSTR_SPI,
    MP_TYPE_FLAG_NONE,
    make_new, spi_make_new,
    print, spi_print,
    locals_dict, &spi_locals_dict
    );
