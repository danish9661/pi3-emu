// BCM2837 guest interrupt plumbing for machine.Pin.irq().
//
// Real delivery path (verified by the lirq guest): the GPIO bank line
// reaches the legacy IC (IRQ 81/82, bank-2 bits 17/18), the GPU line
// reaches the local block, and the host raises CPU_INTERRUPT_HARD. The
// vector stub (vectors.s) saves everything and calls irq_c_handler, which
// acks GPEDS (W1C, de-asserting the level in real time) and queues the
// registered Python callbacks. The queue drains in main-loop context
// (mp_hal_stdin_rx_chr spin), ESP32-style deferred — never inside the
// vector itself.

#include "py/runtime.h"
#include "py/mphal.h"

#define GPIO_BASE (0x3F200000UL)
#define GPLEV0 (*(volatile unsigned *)(GPIO_BASE + 0x34))
#define GPLEV1 (*(volatile unsigned *)(GPIO_BASE + 0x38))
#define GPEDS0 (*(volatile unsigned *)(GPIO_BASE + 0x40))
#define GPEDS1 (*(volatile unsigned *)(GPIO_BASE + 0x44))
#define GPREN0 (*(volatile unsigned *)(GPIO_BASE + 0x4C))
#define GPREN1 (*(volatile unsigned *)(GPIO_BASE + 0x50))
#define GPFEN0 (*(volatile unsigned *)(GPIO_BASE + 0x58))
#define GPFEN1 (*(volatile unsigned *)(GPIO_BASE + 0x5C))
#define IC_ENABLE_IRQS2 (*(volatile unsigned *)(0x3F00B200u + 0x14))

// Pico SDK trigger values (kept for user-code compatibility).
#define IRQ_LOW_LEVEL  (1)
#define IRQ_HIGH_LEVEL (2)
#define IRQ_FALLING    (4)
#define IRQ_RISING     (8)

#define MAX_IRQ_PINS (8)
#define PENDING_DEPTH (8)

typedef struct {
    int pin;
    int trigger;
    mp_obj_t handler;
    mp_obj_t pin_obj;
} irq_reg_t;

static irq_reg_t irq_regs[MAX_IRQ_PINS];
static mp_obj_t pending[PENDING_DEPTH];
static volatile unsigned pending_head = 0;
static volatile unsigned pending_tail = 0;

extern const unsigned char vectors[];

// Called from machine.Pin.irq(). handler MP_OBJ_NULL disables.
void pin_irq_config(int pin, int trigger, mp_obj_t handler, mp_obj_t pin_obj) {
    int i, slot = -1;
    for (i = 0; i < MAX_IRQ_PINS; i++) {
        if (irq_regs[i].handler != MP_OBJ_NULL && irq_regs[i].pin == pin) {
            slot = i;
            break;
        }
        if (slot < 0 && irq_regs[i].handler == MP_OBJ_NULL) {
            slot = i;
        }
    }
    if (handler == MP_OBJ_NULL) {
        if (slot >= 0 && irq_regs[slot].pin == pin) {
            irq_regs[slot].handler = MP_OBJ_NULL;
        }
        return;
    }
    if (slot < 0) {
        mp_raise_msg(&mp_type_OSError, MP_ERROR_TEXT("irq slots full"));
    }
    if (trigger & ~(IRQ_RISING | IRQ_FALLING)) {
        mp_raise_ValueError(MP_ERROR_TEXT("edge triggers only"));
    }
    irq_regs[slot].pin = pin;
    irq_regs[slot].trigger = trigger;
    irq_regs[slot].handler = handler;
    irq_regs[slot].pin_obj = pin_obj;
    if (pin < 32) {
        if (trigger & IRQ_RISING) {
            GPREN0 |= 1u << pin;
        }
        if (trigger & IRQ_FALLING) {
            GPFEN0 |= 1u << pin;
        }
        GPEDS0 = 1u << pin; // clear stale
    } else {
        if (trigger & IRQ_RISING) {
            GPREN1 |= 1u << (pin - 32);
        }
        if (trigger & IRQ_FALLING) {
            GPFEN1 |= 1u << (pin - 32);
        }
        GPEDS1 = 1u << (pin - 32); // clear stale
    }
    // Both GPIO bank lines into the legacy IC (IRQ 81/82); the GPU line
    // carries them to the local block (default routing, like lirq).
    IC_ENABLE_IRQS2 |= (1u << 17) | (1u << 18);
}

// Vector context: ack covered events, queue their callbacks.
void irq_c_handler(void) {
    unsigned p0 = GPEDS0, p1 = GPEDS1;
    int i;
    GPEDS0 = p0; // W1C ack: level de-asserts in real time
    GPEDS1 = p1;
    for (i = 0; i < MAX_IRQ_PINS; i++) {
        if (irq_regs[i].handler == MP_OBJ_NULL) {
            continue;
        }
        int pin = irq_regs[i].pin;
        unsigned bit = pin < 32 ? (p0 >> pin) & 1u : (p1 >> (pin - 32)) & 1u;
        if (!bit) {
            continue;
        }
        // The model raises events for any level change covered by ANY
        // enable; qualify by the pin's live level so RISING fires high
        // and FALLING fires low (also debounces model quirks).
        unsigned lev = pin < 32 ? (GPLEV0 >> pin) & 1u : (GPLEV1 >> (pin - 32)) & 1u;
        int trig = irq_regs[i].trigger;
        if (!((trig & IRQ_RISING) && lev) && !((trig & IRQ_FALLING) && !lev)) {
            continue;
        }
        {
            unsigned next = (pending_head + 1) % PENDING_DEPTH;
            if (next != pending_tail) {
                pending[pending_head] = irq_regs[i].pin_obj;
                pending_head = next;
            }
        }
    }
}

// Drain queued callbacks in main-loop context. Each pending pin_obj is
// resolved back to its handler (registered above) and called as handler(pin).
void irq_drain(void) {
    while (pending_tail != pending_head) {
        mp_obj_t pin_obj = pending[pending_tail];
        pending_tail = (pending_tail + 1) % PENDING_DEPTH;
        mp_obj_t handler = MP_OBJ_NULL;
        // Match by identity against the registry for the callback.
        for (int i = 0; i < MAX_IRQ_PINS; i++) {
            if (irq_regs[i].pin_obj == pin_obj) {
                handler = irq_regs[i].handler;
                break;
            }
        }
        if (handler == MP_OBJ_NULL) {
            continue;
        }
        nlr_buf_t nlr;
        if (nlr_push(&nlr) == 0) {
            mp_call_function_1(handler, pin_obj);
            nlr_pop();
        } else {
            mp_obj_print_exception(&mp_plat_print, MP_OBJ_TO_PTR(nlr.ret_val));
        }
    }
}

void irq_init(void) {
    __asm__ volatile(
        "adrp x0, vectors\n"
        "add x0, x0, :lo12:vectors\n"
        "msr vbar_el1, x0\n"
        "msr daifclr, #2\n"
        ::: "x0");
}
