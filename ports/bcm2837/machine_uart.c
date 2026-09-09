// BCM2837 machine.UART: PL011 (id 0, full duplex) + mini UART (id 1,
// transmit-only, matching the uart1 device model) with Pico-compatible
// constructor and methods (read/readinto/write/any).
//
//   from machine import UART
//   u = UART(0, 115200)
//   u.write(b"hi")
//   u.any()          # bytes waiting
//   u.read(4)        # bytes or None
//
// Notes: baudrate programs real IBRD/FBRD dividers (3 MHz UARTCLK, like the
// uart0 guest); the model does not enforce timing.Consecutive DR reads in
// one slice see the same preloaded cell, so each received byte settles
// across slice boundaries (same slice-staleness class as the I2C/SPI
// drop-syncs). RX on id 1 is unsupported (model is TX-only there).

#include "py/runtime.h"
#include "py/mphal.h"
#include "py/mperrno.h"
#include <stdint.h>

#define UART0_BASE (0x3F201000UL)
#define UART1_BASE (0x3F215000UL)
#define PL011_DR(base)   (*(volatile unsigned *)((base) + 0x00))
#define PL011_FR(base)   (*(volatile unsigned *)((base) + 0x18))
#define PL011_IBRD(base) (*(volatile unsigned *)((base) + 0x24))
#define PL011_FBRD(base) (*(volatile unsigned *)((base) + 0x28))
#define PL011_LCRH(base) (*(volatile unsigned *)((base) + 0x2C))
#define PL011_CR(base)   (*(volatile unsigned *)((base) + 0x30))
#define TMR_CLO (*(volatile unsigned *)(0x3F003000UL + 0x04))

#define FR_RXFE (1u << 4)
#define FR_TXFF (1u << 5)

// Settle spins so a DR read sees a freshly preloaded cell (see header).
static void uart_settle(void) {
    for (volatile unsigned i = 0; i < 3000; i++) {
    }
}

typedef struct _machine_uart_obj_t {
    mp_obj_base_t base;
    uintptr_t regs;
    int timeout_ms; // 0 = return immediately with available bytes
} machine_uart_obj_t;

extern const mp_obj_type_t machine_uart_type;

static uintptr_t uart_base(int id) {
    if (id == 0) {
        return UART0_BASE;
    } else if (id == 1) {
        return UART1_BASE;
    }
    mp_raise_ValueError(MP_ERROR_TEXT("UART id 0..1"));
    return 0;
}

static void uart_configure(uintptr_t base, unsigned baud) {
    // 3 MHz UARTCLK: IBRD/FBRD like the uart0 guest (115200 -> 1/0x28).
    if (baud == 0) {
        baud = 115200;
    }
    unsigned total = (3000000u * 64u + (16u * baud) / 2u) / (16u * baud);
    unsigned div = total / 64;
    unsigned frac = total % 64;
    if (div == 0) {
        div = 1;
    }
    if (div > 0xFFFF) {
        div = 0xFFFF;
    }
    PL011_CR(base) = 0;
    PL011_IBRD(base) = div;
    PL011_FBRD(base) = frac & 0x3F;
    PL011_LCRH(base) = 0x70; // 8N1 + FIFOs
    PL011_CR(base) = 0x301; // UARTEN + TXE + RXE
}

static mp_obj_t uart_make_new(const mp_obj_type_t *type, size_t n_args, size_t n_kw, const mp_obj_t *all_args) {
    enum { ARG_id, ARG_baudrate, ARG_bits, ARG_parity, ARG_stop, ARG_tx, ARG_rx, ARG_timeout };
    static const mp_arg_t allowed[] = {
        { MP_QSTR_id,       MP_ARG_REQUIRED | MP_ARG_INT, {.u_int = 0} },
        { MP_QSTR_baudrate, MP_ARG_INT,                   {.u_int = 115200} },
        { MP_QSTR_bits,     MP_ARG_INT,                   {.u_int = 8} },
        { MP_QSTR_parity,   MP_ARG_OBJ,                   {.u_rom_obj = MP_ROM_NONE} },
        { MP_QSTR_stop,     MP_ARG_INT,                   {.u_int = 1} },
        { MP_QSTR_tx,       MP_ARG_OBJ,                   {.u_rom_obj = MP_ROM_NONE} },
        { MP_QSTR_rx,       MP_ARG_OBJ,                   {.u_rom_obj = MP_ROM_NONE} },
        { MP_QSTR_timeout,  MP_ARG_KW_ONLY | MP_ARG_INT,  {.u_int = 0} },
    };
    mp_arg_val_t args[MP_ARRAY_SIZE(allowed)];
    mp_arg_parse_all_kw_array(n_args, n_kw, all_args, MP_ARRAY_SIZE(allowed), allowed, args);
    int id = args[ARG_id].u_int;
    machine_uart_obj_t *self = mp_obj_malloc(machine_uart_obj_t, &machine_uart_type);
    self->regs = uart_base(id);
    self->timeout_ms = args[ARG_timeout].u_int;
    if (args[ARG_bits].u_int != 8) {
        mp_raise_ValueError(MP_ERROR_TEXT("8 bits only"));
    }
    // tx/rx pin ids accepted and ignored (fixed SoC wiring).
    uart_configure(self->regs, (unsigned)args[ARG_baudrate].u_int);
    return MP_OBJ_FROM_PTR(self);
}

static void uart_print(const mp_print_t *print, mp_obj_t self_in, mp_print_kind_t kind) {
    machine_uart_obj_t *self = MP_OBJ_TO_PTR(self_in);
    mp_printf(print, "UART(%u)", self->regs == UART0_BASE ? 0 : 1);
}

// Bytes currently readable without blocking (0/1: the model preloads one).
static mp_obj_t uart_any(mp_obj_t self_in) {
    machine_uart_obj_t *self = MP_OBJ_TO_PTR(self_in);
    if (self->regs != UART0_BASE) {
        return MP_OBJ_NEW_SMALL_INT(0);
    }
    return MP_OBJ_NEW_SMALL_INT((PL011_FR(self->regs) & FR_RXFE) ? 0 : 1);
}
static MP_DEFINE_CONST_FUN_OBJ_1(uart_any_obj, uart_any);

static int uart_getc(machine_uart_obj_t *self) {
    uintptr_t base = self->regs;
    if (base != UART0_BASE) {
        return -1;
    }
    if (PL011_FR(base) & FR_RXFE) {
        return -1;
    }
    int c = PL011_DR(base) & 0xFF;
    uart_settle();
    return c;
}

static mp_obj_t uart_read(size_t n_args, const mp_obj_t *args) {
    machine_uart_obj_t *self = MP_OBJ_TO_PTR(args[0]);
    size_t len = n_args > 1 ? (size_t)mp_obj_get_int(args[1]) : 1;
    unsigned start_ms = TMR_CLO / 1000;
    vstr_t vstr;
    vstr_init_len(&vstr, len ? len : 1);
    size_t got = 0;
    while (got < len) {
        int c = uart_getc(self);
        if (c < 0) {
            // timeout_ms == 0 returns immediately with what's available.
            if (TMR_CLO / 1000 - start_ms >= (unsigned)self->timeout_ms) {
                break;
            }
            continue;
        }
        vstr.buf[got++] = (char)c;
    }
    if (got == 0) {
        return mp_const_none;
    }
    vstr.len = got;
    return mp_obj_new_bytes_from_vstr(&vstr);
}
static MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN(uart_read_obj, 1, 2, uart_read);

static mp_obj_t uart_readinto(size_t n_args, const mp_obj_t *args) {
    machine_uart_obj_t *self = MP_OBJ_TO_PTR(args[0]);
    mp_buffer_info_t bufinfo;
    mp_get_buffer_raise(args[1], &bufinfo, MP_BUFFER_WRITE);
    size_t got = 0;
    while (got < bufinfo.len) {
        int c = uart_getc(self);
        if (c < 0) {
            break;
        }
        ((uint8_t *)bufinfo.buf)[got++] = (uint8_t)c;
    }
    return MP_OBJ_NEW_SMALL_INT(got);
}
static MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN(uart_readinto_obj, 2, 3, uart_readinto);

static mp_obj_t uart_write(size_t n_args, const mp_obj_t *args) {
    machine_uart_obj_t *self = MP_OBJ_TO_PTR(args[0]);
    uintptr_t base = self->regs;
    mp_buffer_info_t bufinfo;
    mp_get_buffer_raise(args[1], &bufinfo, MP_BUFFER_READ);
    for (size_t i = 0; i < bufinfo.len; i++) {
        while (PL011_FR(base) & FR_TXFF) {
        }
        if (base == UART0_BASE) {
            PL011_DR(base) = ((uint8_t *)bufinfo.buf)[i];
        } else {
            // Mini UART data register (AUX_MU_IO).
            *(volatile unsigned *)(base + 0x40) = ((uint8_t *)bufinfo.buf)[i];
        }
    }
    return MP_OBJ_NEW_SMALL_INT(bufinfo.len);
}
static MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN(uart_write_obj, 2, 3, uart_write);

static const mp_rom_map_elem_t uart_locals_dict_table[] = {
    { MP_ROM_QSTR(MP_QSTR_read), MP_ROM_PTR(&uart_read_obj) },
    { MP_ROM_QSTR(MP_QSTR_readinto), MP_ROM_PTR(&uart_readinto_obj) },
    { MP_ROM_QSTR(MP_QSTR_write), MP_ROM_PTR(&uart_write_obj) },
    { MP_ROM_QSTR(MP_QSTR_any), MP_ROM_PTR(&uart_any_obj) },
};
static MP_DEFINE_CONST_DICT(uart_locals_dict, uart_locals_dict_table);

MP_DEFINE_CONST_OBJ_TYPE(
    machine_uart_type,
    MP_QSTR_UART,
    MP_TYPE_FLAG_NONE,
    make_new, uart_make_new,
    print, uart_print,
    locals_dict, &uart_locals_dict
    );
