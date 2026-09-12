// machine.Timer for the bcm2837 port (Pico-compatible construction):
//
//   from machine import Timer
//   t = Timer(1, mode=Timer.PERIODIC, period=500,
//             callback=lambda t: print("tick"))
//   t.deinit()
//
// Timer id 0..3 maps to system-timer channels C0..C3 (compare = CLO +
// period, CS bit acked W1C, IRQ 1 through the legacy IC into the local
// block). The vector (irq.c) re-arms PERIODIC channels and queues the
// callback; irq_drain runs it in main-loop context, like Pin.irq — never
// inside the vector itself. period is milliseconds (>= 1).

#include "py/runtime.h"
#include "py/mphal.h"

#define TIMER_PERIODIC (1)
#define TIMER_ONE_SHOT (0)

extern void timer_config(int ch, unsigned period_us, int mode, mp_obj_t handler, mp_obj_t timer_obj);
extern void timer_stop(int ch);

typedef struct _machine_timer_obj_t {
    mp_obj_base_t base;
    int id;
} machine_timer_obj_t;

static void timer_apply(machine_timer_obj_t *self, int mode, int period, mp_obj_t cb) {
    if (mode != TIMER_PERIODIC && mode != TIMER_ONE_SHOT) {
        mp_raise_ValueError(MP_ERROR_TEXT("bad mode"));
    }
    if (cb == MP_OBJ_NULL || period < 0) {
        timer_stop(self->id);
        return;
    }
    if (period == 0) {
        mp_raise_ValueError(MP_ERROR_TEXT("bad period"));
    }
    timer_config(self->id, (unsigned)period * 1000u, mode, cb, MP_OBJ_FROM_PTR(self));
}

static mp_obj_t machine_timer_make_new(const mp_obj_type_t *type, size_t n_args, size_t n_kw, const mp_obj_t *args) {
    enum { ARG_id, ARG_mode, ARG_period, ARG_callback };
    static const mp_arg_t allowed_args[] = {
        { MP_QSTR_id, MP_ARG_REQUIRED | MP_ARG_INT, {.u_int = 0} },
        { MP_QSTR_mode, MP_ARG_INT, {.u_int = TIMER_PERIODIC} },
        { MP_QSTR_period, MP_ARG_INT, {.u_int = -1} },
        { MP_QSTR_callback, MP_ARG_OBJ, {.u_obj = MP_OBJ_NULL} },
    };
    mp_arg_val_t vals[MP_ARRAY_SIZE(allowed_args)];
    mp_arg_parse_all_kw_array(n_args, n_kw, args, MP_ARRAY_SIZE(allowed_args), allowed_args, vals);
    int id = vals[ARG_id].u_int;
    if (id < 0 || id > 3) {
        mp_raise_ValueError(MP_ERROR_TEXT("bad timer id"));
    }
    machine_timer_obj_t *self = mp_obj_malloc(machine_timer_obj_t, type);
    self->id = id;
    timer_apply(self, vals[ARG_mode].u_int, vals[ARG_period].u_int, vals[ARG_callback].u_obj);
    return MP_OBJ_FROM_PTR(self);
}

static void machine_timer_print(const mp_print_t *print, mp_obj_t self_in, mp_print_kind_t kind) {
    machine_timer_obj_t *self = MP_OBJ_TO_PTR(self_in);
    mp_printf(print, "Timer(%d)", self->id);
}

static mp_obj_t machine_timer_init(size_t n_args, const mp_obj_t *pos_args, mp_map_t *kw_args) {
    enum { ARG_mode, ARG_period, ARG_callback };
    static const mp_arg_t allowed_args[] = {
        { MP_QSTR_mode, MP_ARG_KW_ONLY | MP_ARG_INT, {.u_int = TIMER_PERIODIC} },
        { MP_QSTR_period, MP_ARG_KW_ONLY | MP_ARG_INT, {.u_int = -1} },
        { MP_QSTR_callback, MP_ARG_KW_ONLY | MP_ARG_OBJ, {.u_obj = MP_OBJ_NULL} },
    };
    machine_timer_obj_t *self = MP_OBJ_TO_PTR(pos_args[0]);
    mp_arg_val_t vals[MP_ARRAY_SIZE(allowed_args)];
    mp_arg_parse_all(n_args - 1, pos_args + 1, kw_args, MP_ARRAY_SIZE(allowed_args), allowed_args, vals);
    timer_apply(self, vals[ARG_mode].u_int, vals[ARG_period].u_int, vals[ARG_callback].u_obj);
    return mp_const_none;
}
MP_DEFINE_CONST_FUN_OBJ_KW(machine_timer_init_obj, 1, machine_timer_init);

static mp_obj_t machine_timer_deinit(mp_obj_t self_in) {
    machine_timer_obj_t *self = MP_OBJ_TO_PTR(self_in);
    timer_stop(self->id);
    return mp_const_none;
}
MP_DEFINE_CONST_FUN_OBJ_1(machine_timer_deinit_obj, machine_timer_deinit);

static const mp_rom_map_elem_t machine_timer_locals_dict_table[] = {
    { MP_ROM_QSTR(MP_QSTR_init), MP_ROM_PTR(&machine_timer_init_obj) },
    { MP_ROM_QSTR(MP_QSTR_deinit), MP_ROM_PTR(&machine_timer_deinit_obj) },
    { MP_ROM_QSTR(MP_QSTR_PERIODIC), MP_ROM_INT(TIMER_PERIODIC) },
    { MP_ROM_QSTR(MP_QSTR_ONE_SHOT), MP_ROM_INT(TIMER_ONE_SHOT) },
};
static MP_DEFINE_CONST_DICT(machine_timer_locals_dict, machine_timer_locals_dict_table);

MP_DEFINE_CONST_OBJ_TYPE(
    machine_timer_type,
    MP_QSTR_Timer,
    MP_TYPE_FLAG_NONE,
    make_new, machine_timer_make_new,
    print, machine_timer_print,
    locals_dict, &machine_timer_locals_dict
    );
