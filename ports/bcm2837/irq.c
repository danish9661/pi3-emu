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
#define GPHEN0 (*(volatile unsigned *)(GPIO_BASE + 0x64))
#define GPHEN1 (*(volatile unsigned *)(GPIO_BASE + 0x68))
#define GPLEN0 (*(volatile unsigned *)(GPIO_BASE + 0x70))
#define GPLEN1 (*(volatile unsigned *)(GPIO_BASE + 0x74))
#define IC_ENABLE_IRQS2 (*(volatile unsigned *)(0x3F00B200u + 0x14))
#define IC_ENABLE_IRQS1 (*(volatile unsigned *)(0x3F00B200u + 0x10))
#define TMR_BASE (0x3F003000UL)
// System-timer CS ack quirk (load-bearing): the host model treats a CS
// write as a KEEP mask (pending &= value), inverted vs hardware W1C — the
// bare-metal guests work around it with `str wzr` (clear-all). The Timer
// driver needs per-channel precision, so it writes the complement: to
// clear bit i only, store 0xF ^ (1<<i) (keeps the other three).
#define TMR_CS_ACK(i) (0xFu ^ (1u << (i)))
#define TMR_CS (*(volatile unsigned *)(TMR_BASE + 0x00))
#define TMR_CLO (*(volatile unsigned *)(TMR_BASE + 0x04))
#define TMR_CMP(ch) (*(volatile unsigned *)(TMR_BASE + 0x0C + 4 * (ch)))

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

// System-timer channels (machine.Timer id 0..3 <-> C0..C3): period in
// microseconds, re-armed in the vector for PERIODIC.
#define MAX_TIMERS (4)
typedef struct {
    unsigned period_us;
    int mode; // 0 one-shot, 1 periodic
    mp_obj_t handler;
    mp_obj_t timer_obj;
} timer_reg_t;

static timer_reg_t timer_regs[MAX_TIMERS];

static void pending_push(mp_obj_t o) {
    unsigned next = (pending_head + 1) % PENDING_DEPTH;
    if (next != pending_tail) {
        pending[pending_head] = o;
        pending_head = next;
    }
}

// Called from machine.Timer.init(). period_us wraps mod 2^32 (CLO is a
// 32-bit microsecond counter); the host model matches the same way.
void timer_config(int ch, unsigned period_us, int mode, mp_obj_t handler, mp_obj_t timer_obj) {
    timer_regs[ch].period_us = period_us;
    timer_regs[ch].mode = mode;
    timer_regs[ch].handler = handler;
    timer_regs[ch].timer_obj = timer_obj;
    TMR_CMP(ch) = TMR_CLO + period_us;
    TMR_CS = TMR_CS_ACK(ch); // clear any stale match on this channel only
    IC_ENABLE_IRQS1 |= 1u << ch; // channel-n match -> bank-1 bit n (IRQ n)
}

// Called from machine.Timer.deinit(): drop the registration, its queued
// callbacks, and any latched match.
void timer_stop(int ch) {
    timer_regs[ch].handler = MP_OBJ_NULL;
    unsigned w = pending_tail;
    unsigned r = pending_tail;
    while (r != pending_head) {
        if (pending[r] != timer_regs[ch].timer_obj) {
            pending[w] = pending[r];
            w = (w + 1) % PENDING_DEPTH;
        }
        r = (r + 1) % PENDING_DEPTH;
    }
    pending_head = w;
    timer_regs[ch].timer_obj = MP_OBJ_NULL;
    TMR_CS = TMR_CS_ACK(ch); // drop its latched match, keep other channels
}

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
            // Disable decodes fully: clear the pin's enable bits (a stale
            // GPHEN/GPLEN would keep the model raising), flush its queued
            // events, then drop the handler.
            if (pin < 32) {
                GPREN0 &= ~(1u << pin);
                GPFEN0 &= ~(1u << pin);
                GPHEN0 &= ~(1u << pin);
                GPLEN0 &= ~(1u << pin);
            } else {
                GPREN1 &= ~(1u << (pin - 32));
                GPFEN1 &= ~(1u << (pin - 32));
                GPHEN1 &= ~(1u << (pin - 32));
                GPLEN1 &= ~(1u << (pin - 32));
            }
            unsigned w = pending_tail;
            unsigned r = pending_tail;
            while (r != pending_head) {
                mp_obj_t o = pending[r];
                r = (r + 1) % PENDING_DEPTH;
                int op = -1;
                for (int j = 0; j < MAX_IRQ_PINS; j++) {
                    if (irq_regs[j].pin_obj == o) {
                        op = irq_regs[j].pin;
                        break;
                    }
                }
                if (op != pin) {
                    pending[w] = o;
                    w = (w + 1) % PENDING_DEPTH;
                }
            }
            pending_head = w;
            irq_regs[slot].handler = MP_OBJ_NULL;
        }
        return;
    }
    if (slot < 0) {
        mp_raise_msg(&mp_type_OSError, MP_ERROR_TEXT("irq slots full"));
    }
    if (trigger & ~(IRQ_RISING | IRQ_FALLING | IRQ_LOW_LEVEL | IRQ_HIGH_LEVEL)) {
        mp_raise_ValueError(MP_ERROR_TEXT("bad trigger"));
    }
    irq_regs[slot].pin = pin;
    irq_regs[slot].trigger = trigger;
    irq_regs[slot].handler = handler;
    irq_regs[slot].pin_obj = pin_obj;
    // Fresh enables: a re-arm replaces the trigger, so clear stale bits
    // first (see disable path above).
    if (pin < 32) {
        GPREN0 &= ~(1u << pin);
        GPFEN0 &= ~(1u << pin);
        GPHEN0 &= ~(1u << pin);
        GPLEN0 &= ~(1u << pin);
        if (trigger & IRQ_RISING) {
            GPREN0 |= 1u << pin;
        }
        if (trigger & IRQ_FALLING) {
            GPFEN0 |= 1u << pin;
        }
        if (trigger & IRQ_HIGH_LEVEL) {
            GPHEN0 |= 1u << pin;
        }
        if (trigger & IRQ_LOW_LEVEL) {
            GPLEN0 |= 1u << pin;
        }
        GPEDS0 = 1u << pin; // clear stale
    } else {
        GPREN1 &= ~(1u << (pin - 32));
        GPFEN1 &= ~(1u << (pin - 32));
        GPHEN1 &= ~(1u << (pin - 32));
        GPLEN1 &= ~(1u << (pin - 32));
        if (trigger & IRQ_RISING) {
            GPREN1 |= 1u << (pin - 32);
        }
        if (trigger & IRQ_FALLING) {
            GPFEN1 |= 1u << (pin - 32);
        }
        if (trigger & IRQ_HIGH_LEVEL) {
            GPHEN1 |= 1u << (pin - 32);
        }
        if (trigger & IRQ_LOW_LEVEL) {
            GPLEN1 |= 1u << (pin - 32);
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
    // System-timer channels first (machine.Timer): ack the match, re-arm
    // PERIODIC from the latched compare (wrap-safe u32), queue the tick.
    // The ack runs for DISARMED channels too: a stale compare (left armed
    // by deinit) legitimately fires once, and without an ack its level
    // would livelock the REPL.
    unsigned cs = TMR_CS;
    for (i = 0; i < MAX_TIMERS; i++) {
        if (!(cs & (1u << i))) {
            continue;
        }
        TMR_CS = TMR_CS_ACK(i); // per-bit clear (see quirk above)
        if (timer_regs[i].handler == MP_OBJ_NULL) {
            continue;
        }
        if (timer_regs[i].mode) {
            TMR_CMP(i) += timer_regs[i].period_us;
        }
        pending_push(timer_regs[i].timer_obj);
    }
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
        // enable; qualify by the pin's live level so each trigger only
        // fires on its own condition (also debounces model quirks).
        unsigned lev = pin < 32 ? (GPLEV0 >> pin) & 1u : (GPLEV1 >> (pin - 32)) & 1u;
        int trig = irq_regs[i].trigger;
        int fire = ((trig & IRQ_RISING) && lev) || ((trig & IRQ_FALLING) && !lev) ||
            ((trig & IRQ_HIGH_LEVEL) && lev) || ((trig & IRQ_LOW_LEVEL) && !lev);
        if (!fire) {
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
// Timer ticks resolve to their Timer callback instead (called as handler(timer)).
void irq_drain(void) {
    while (pending_tail != pending_head) {
        mp_obj_t pin_obj = pending[pending_tail];
        pending_tail = (pending_tail + 1) % PENDING_DEPTH;
        mp_obj_t handler = MP_OBJ_NULL;
        // Match timers first (their objs never collide with Pin objs:
        // each Timer make_new allocates a fresh object).
        for (int i = 0; i < MAX_TIMERS; i++) {
            if (timer_regs[i].timer_obj == pin_obj && timer_regs[i].handler != MP_OBJ_NULL) {
                handler = timer_regs[i].handler;
                break;
            }
        }
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
