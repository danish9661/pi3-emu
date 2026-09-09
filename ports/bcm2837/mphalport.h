#include "py/mpconfig.h"
#include "py/mphal.h"

// Port HAL declarations.
int mp_hal_stdin_rx_chr(void);
mp_uint_t mp_hal_stdout_tx_strn(const char *str, size_t len);
mp_uint_t mp_hal_ticks_ms(void);
void mp_hal_delay_ms(mp_uint_t ms);
void uart_init(void);
void mp_hal_set_interrupt_char(int c);
void irq_init(void);
void irq_drain(void);
