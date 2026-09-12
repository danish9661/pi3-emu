// BCM2837 machine.Pin: GPIO pins 0..53 via the real register file.
//
//   from machine import Pin
//   led = Pin(21, Pin.OUT)
//   led.on() / led.off() / led.value()
//   btn = Pin(29, Pin.IN)
//   btn.value()          # 1 while the host holds BTN 29
//
// Pin numbers are BCM (SoC) numbers, matching Pico-style machine code that
// uses integer ids. irq() is not yet implemented (needs guest-side vector
// plumbing); ALT functions are rejected. Pulls run the real GPPUD sequence.

#include "py/runtime.h"
#include "py/mphal.h"

#define GPIO_BASE   (0x3F200000UL)
#define GPIO_REG(o) (*(volatile unsigned *)(GPIO_BASE + (o)))
#define GPFSEL(n)   GPIO_REG((n) * 4)
#define GPSET0      GPIO_REG(0x1C)
#define GPSET1      GPIO_REG(0x20)
#define GPCLR0      GPIO_REG(0x28)
#define GPCLR1      GPIO_REG(0x2C)
#define GPLEV0      GPIO_REG(0x34)
#define GPLEV1      GPIO_REG(0x38)
#define GPPUD       GPIO_REG(0x94)
#define GPPUDCLK0   GPIO_REG(0x98)
#define GPPUDCLK1   GPIO_REG(0x9C)

// Must match the values exposed as Pin.IN/OUT/... below.
enum {
    PIN_MODE_IN = 0,
    PIN_MODE_OUT = 1,
    PIN_MODE_OPEN_DRAIN = 2,
    PIN_MODE_ALT = 3,
};

enum {
    PIN_PULL_NONE = 0,
    PIN_PULL_UP = 1,
    PIN_PULL_DOWN = 2,
};

typedef struct _machine_pin_obj_t {
    mp_obj_base_t base;
    int id;
} machine_pin_obj_t;

extern const mp_obj_type_t machine_pin_type;

static void pin_check_id(int id) {
    if (id < 0 || id > 53) {
        mp_raise_ValueError(MP_ERROR_TEXT("pin out of range 0..53"));
    }
}

// The GPPUD programming sequence needs ~150-cycle waits; a short calibrated
// busy loop is plenty at emulated speed (and harmless on silicon).
static void short_delay(void) {
    for (volatile int i = 0; i < 1000; i++) {
    }
}

static void pin_set_function(int id, int fn) {
    volatile unsigned *fsel = &GPFSEL(id / 10);
    unsigned shift = (unsigned)(id % 10) * 3;
    *fsel = (*fsel & ~(7u << shift)) | ((unsigned)(fn & 7) << shift);
}

static void pin_set_pull(int id, int pull) {
    // BCM encoding: 0 off, 1 pull-down, 2 pull-up.
    unsigned pud = pull == PIN_PULL_UP ? 2 : pull == PIN_PULL_DOWN ? 1 : 0;
    volatile unsigned *clk = (id < 32) ? &GPPUDCLK0 : &GPPUDCLK1;
    unsigned mask = 1u << (id % 32);
    GPPUD = pud;
    short_delay();
    *clk = mask;
    short_delay();
    GPPUD = 0;
    *clk = 0;
}

static void pin_output(int id, int v) {
    if (v) {
        if (id < 32) { GPSET0 = 1u << id; } else { GPSET1 = 1u << (id - 32); }
    } else {
        if (id < 32) { GPCLR0 = 1u << id; } else { GPCLR1 = 1u << (id - 32); }
    }
}

static int pin_input(int id) {
    unsigned lev = (id < 32) ? GPLEV0 : GPLEV1;
    return (lev >> (id % 32)) & 1;
}

static void pin_init_helper(machine_pin_obj_t *self, int mode, int pull) {
    if (mode == PIN_MODE_OUT) {
        pin_set_function(self->id, 1);
    } else if (mode == PIN_MODE_IN) {
        pin_set_function(self->id, 0);
    } else {
        mp_raise_ValueError(MP_ERROR_TEXT("mode not supported (IN/OUT only)"));
    }
    if (pull < 0) {
        pull = PIN_PULL_NONE;
    }
    pin_set_pull(self->id, pull);
}

enum { ARG_id, ARG_mode, ARG_pull, ARG_value };
static const mp_arg_t pin_make_new_allowed[] = {
    { MP_QSTR_id,    MP_ARG_REQUIRED | MP_ARG_INT,  {.u_int = 0} },
    { MP_QSTR_mode,  MP_ARG_INT,                    {.u_int = PIN_MODE_IN} },
    { MP_QSTR_pull,  MP_ARG_INT,                    {.u_int = -1} },
    { MP_QSTR_value, MP_ARG_KW_ONLY | MP_ARG_OBJ,   {.u_rom_obj = MP_ROM_NONE} },
};

static mp_obj_t pin_make_new(const mp_obj_type_t *type, size_t n_args, size_t n_kw, const mp_obj_t *args) {
    mp_arg_val_t vals[MP_ARRAY_SIZE(pin_make_new_allowed)];
    mp_arg_parse_all_kw_array(n_args, n_kw, args, MP_ARRAY_SIZE(pin_make_new_allowed), pin_make_new_allowed, vals);
    int id = vals[ARG_id].u_int;
    pin_check_id(id);
    machine_pin_obj_t *self = mp_obj_malloc(machine_pin_obj_t, &machine_pin_type);
    self->id = id;
    pin_init_helper(self, vals[ARG_mode].u_int, vals[ARG_pull].u_int);
    if (vals[ARG_value].u_obj != mp_const_none) {
        pin_output(self->id, mp_obj_is_true(vals[ARG_value].u_obj));
    }
    return MP_OBJ_FROM_PTR(self);
}

static void pin_print(const mp_print_t *print, mp_obj_t self_in, mp_print_kind_t kind) {
    machine_pin_obj_t *self = MP_OBJ_TO_PTR(self_in);
    mp_printf(print, "Pin(%d)", self->id);
}

static mp_obj_t pin_value(size_t n_args, const mp_obj_t *args) {
    machine_pin_obj_t *self = MP_OBJ_TO_PTR(args[0]);
    if (n_args == 1) {
        return mp_obj_new_bool(pin_input(self->id));
    }
    pin_output(self->id, mp_obj_is_true(args[1]));
    return mp_const_none;
}
static MP_DEFINE_CONST_FUN_OBJ_VAR_BETWEEN(pin_value_obj, 1, 2, pin_value);

static mp_obj_t pin_on(mp_obj_t self_in) {
    machine_pin_obj_t *self = MP_OBJ_TO_PTR(self_in);
    pin_output(self->id, 1);
    return mp_const_none;
}
static MP_DEFINE_CONST_FUN_OBJ_1(pin_on_obj, pin_on);

static mp_obj_t pin_off(mp_obj_t self_in) {
    machine_pin_obj_t *self = MP_OBJ_TO_PTR(self_in);
    pin_output(self->id, 0);
    return mp_const_none;
}
static MP_DEFINE_CONST_FUN_OBJ_1(pin_off_obj, pin_off);

enum { ARG_init_mode, ARG_init_pull };
static const mp_arg_t pin_init_allowed[] = {
    { MP_QSTR_mode, MP_ARG_INT, {.u_int = PIN_MODE_IN} },
    { MP_QSTR_pull, MP_ARG_INT, {.u_int = -1} },
};

static mp_obj_t pin_init(size_t n_args, const mp_obj_t *args, mp_map_t *kw_args) {    machine_pin_obj_t *self = MP_OBJ_TO_PTR(args[0]);
    mp_arg_val_t vals[MP_ARRAY_SIZE(pin_init_allowed)];
    mp_arg_parse_all(n_args - 1, args + 1, kw_args, MP_ARRAY_SIZE(pin_init_allowed), pin_init_allowed, vals);
    pin_init_helper(self, vals[ARG_init_mode].u_int, vals[ARG_init_pull].u_int);
    return mp_const_none;
}
static MP_DEFINE_CONST_FUN_OBJ_KW(pin_init_obj, 1, pin_init);

static mp_obj_t pin_irq(size_t n_args, const mp_obj_t *pos_args, mp_map_t *kw_args) {
    enum { ARG_handler, ARG_trigger, ARG_hard };
    static const mp_arg_t allowed[] = {
        { MP_QSTR_handler, MP_ARG_OBJ, {.u_rom_obj = MP_ROM_NONE} },
        { MP_QSTR_trigger, MP_ARG_INT, {.u_int = 8} },
        { MP_QSTR_hard,    MP_ARG_BOOL, {.u_bool = false} },
    };
    mp_arg_val_t args[MP_ARRAY_SIZE(allowed)];
    mp_arg_parse_all(n_args - 1, pos_args + 1, kw_args, MP_ARRAY_SIZE(allowed), allowed, args);
    machine_pin_obj_t *self = MP_OBJ_TO_PTR(pos_args[0]);
    // Implemented in irq.c (GPREN/GPFEN arm + deferred dispatch).
    extern void pin_irq_config(int pin, int trigger, mp_obj_t handler, mp_obj_t pin_obj);
    pin_irq_config(self->id, args[ARG_trigger].u_int,
        args[ARG_handler].u_obj == mp_const_none ? MP_OBJ_NULL : args[ARG_handler].u_obj,
        MP_OBJ_FROM_PTR(self));
    return mp_const_none;
}
static MP_DEFINE_CONST_FUN_OBJ_KW(pin_irq_obj, 1, pin_irq);

static const mp_rom_map_elem_t pin_locals_dict_table[] = {
    { MP_ROM_QSTR(MP_QSTR_value), MP_ROM_PTR(&pin_value_obj) },
    { MP_ROM_QSTR(MP_QSTR_on), MP_ROM_PTR(&pin_on_obj) },
    { MP_ROM_QSTR(MP_QSTR_off), MP_ROM_PTR(&pin_off_obj) },
    { MP_ROM_QSTR(MP_QSTR_init), MP_ROM_PTR(&pin_init_obj) },
    { MP_ROM_QSTR(MP_QSTR_irq), MP_ROM_PTR(&pin_irq_obj) },
    { MP_ROM_QSTR(MP_QSTR_IN), MP_ROM_INT(PIN_MODE_IN) },
    { MP_ROM_QSTR(MP_QSTR_OUT), MP_ROM_INT(PIN_MODE_OUT) },
    { MP_ROM_QSTR(MP_QSTR_OPEN_DRAIN), MP_ROM_INT(PIN_MODE_OPEN_DRAIN) },
    { MP_ROM_QSTR(MP_QSTR_ALT), MP_ROM_INT(PIN_MODE_ALT) },
    { MP_ROM_QSTR(MP_QSTR_PULL_UP), MP_ROM_INT(PIN_PULL_UP) },
    { MP_ROM_QSTR(MP_QSTR_PULL_DOWN), MP_ROM_INT(PIN_PULL_DOWN) },
    { MP_ROM_QSTR(MP_QSTR_IRQ_RISING), MP_ROM_INT(8) },
    { MP_ROM_QSTR(MP_QSTR_IRQ_FALLING), MP_ROM_INT(4) },
    { MP_ROM_QSTR(MP_QSTR_IRQ_LOW_LEVEL), MP_ROM_INT(1) },
    { MP_ROM_QSTR(MP_QSTR_IRQ_HIGH_LEVEL), MP_ROM_INT(2) },
};
static MP_DEFINE_CONST_DICT(pin_locals_dict, pin_locals_dict_table);

MP_DEFINE_CONST_OBJ_TYPE(
    machine_pin_type,
    MP_QSTR_Pin,
    MP_TYPE_FLAG_NONE,
    make_new, pin_make_new,
    print, pin_print,
    locals_dict, &pin_locals_dict
    );

// I2C/SPI types live in machine_i2c.c / machine_spi.c (shared extmod
// protocol + BSC/SPI0 drivers).
extern const mp_obj_type_t machine_i2c_type;
extern const mp_obj_type_t machine_spi_type;
extern const mp_obj_type_t machine_uart_type;
extern const mp_obj_type_t machine_timer_type;
extern const mp_obj_t machine_mem32_obj;

static const mp_rom_map_elem_t machine_module_globals_table[] = {
    { MP_ROM_QSTR(MP_QSTR___name__), MP_ROM_QSTR(MP_QSTR_machine) },
    { MP_ROM_QSTR(MP_QSTR_Pin), MP_ROM_PTR(&machine_pin_type) },
    { MP_ROM_QSTR(MP_QSTR_I2C), MP_ROM_PTR(&machine_i2c_type) },
    { MP_ROM_QSTR(MP_QSTR_SPI), MP_ROM_PTR(&machine_spi_type) },
    { MP_ROM_QSTR(MP_QSTR_UART), MP_ROM_PTR(&machine_uart_type) },
    { MP_ROM_QSTR(MP_QSTR_Timer), MP_ROM_PTR(&machine_timer_type) },
    { MP_ROM_QSTR(MP_QSTR_mem32), MP_ROM_PTR(&machine_mem32_obj) },
};
static MP_DEFINE_CONST_DICT(machine_module_globals, machine_module_globals_table);

const mp_obj_module_t machine_module = {
    .base = { &mp_type_module },
    .globals = (mp_obj_dict_t *)&machine_module_globals,
};

MP_REGISTER_MODULE(MP_QSTR_machine, machine_module);
