// BCM2837 port HAL: PL011 UART console + microsecond timer.

#include "py/mpconfig.h"
#include "py/mphal.h"

#define UART0_BASE (0x3F201000u)
#define UART_DR    (*(volatile unsigned *) (UART0_BASE + 0x00))
#define UART_FR    (*(volatile unsigned *) (UART0_BASE + 0x18))
#define UART_IBRD  (*(volatile unsigned *) (UART0_BASE + 0x24))
#define UART_FBRD  (*(volatile unsigned *) (UART0_BASE + 0x28))
#define UART_LCRH  (*(volatile unsigned *) (UART0_BASE + 0x2C))
#define UART_CR    (*(volatile unsigned *) (UART0_BASE + 0x30))

#define FR_RXFE (1u << 4)
#define FR_TXFF (1u << 5)

#define TMR_CLO (*(volatile unsigned *) (0x3F003000u + 0x04))

// Real-PL011-style init (matches the uart0 guest: 115200 8N1, FIFOs on).
void uart_init(void) {
    UART_CR = 0;
    UART_IBRD = 1;
    UART_FBRD = 28;
    UART_LCRH = 0x70;
    UART_CR = 0x301;
}

int mp_hal_stdin_rx_chr(void) {
    // Deferred IRQ dispatch runs here (main-loop context, ESP32-style):
    // vectors set pending flags async, callbacks fire while waiting.
    for (;;) {
        irq_drain();
        if (!(UART_FR & FR_RXFE)) {
            break;
        }
    }
    return (int)(UART_DR & 0xFF);
}

// No keyboard-interrupt plumbing on the spike (Ctrl-C char accepted,
// never raised). Declared here; pyexec calls it at REPL start.
void mp_hal_set_interrupt_char(int c) {
    (void)c;
}

mp_uint_t mp_hal_stdout_tx_strn(const char *str, size_t len) {
    while (len--) {
        while (UART_FR & FR_TXFF) {
        }
        UART_DR = (unsigned)(*str++);
    }
    return 0;
}

mp_uint_t mp_hal_ticks_ms(void) {
    return TMR_CLO / 1000;
}

mp_uint_t mp_hal_ticks_us(void) {
    return TMR_CLO;
}

// No separate CPU counter on the spike: same 1 MHz system-timer source.
mp_uint_t mp_hal_ticks_cpu(void) {
    return TMR_CLO;
}

void mp_hal_delay_ms(mp_uint_t ms) {
    mp_uint_t start = mp_hal_ticks_ms();
    while (mp_hal_ticks_ms() - start < ms) {
    }
}

void mp_hal_delay_us(mp_uint_t us) {
    unsigned start = TMR_CLO;
    while (TMR_CLO - start < us) {
    }
}
