#include <stdint.h>

// options to control how MicroPython is built

// Use the minimal starting configuration (disables all optional features).
#define MICROPY_CONFIG_ROM_LEVEL (MICROPY_CONFIG_ROM_LEVEL_MINIMUM)

// REPL needs the built-in compiler.
#define MICROPY_ENABLE_COMPILER     (1)

#define MICROPY_QSTR_EXTRA_POOL           mp_qstr_frozen_const_pool
#define MICROPY_ENABLE_GC                 (1)
#define MICROPY_HELPER_REPL               (1)
#define MICROPY_MODULE_FROZEN_MPY         (1)
#define MICROPY_PY_MACHINE_I2C            (1)
#define MICROPY_PY_MACHINE_SPI            (1)
// External import ON even without a filesystem: it enables the frozen-module
// search in __import__ (with it off, frozen .mpy modules can never load and
// mp_find_frozen_module is gc'd as unreferenced). File imports fail cleanly
// via the mp_import_stat stub in main.c.
#define MICROPY_ENABLE_EXTERNAL_IMPORT    (1)

#define MICROPY_ALLOC_PATH_MAX            (256)

// Use the minimum headroom in the chunk allocator for parse nodes.
#define MICROPY_ALLOC_PARSE_CHUNK_INIT    (16)

// Floats via soft-float (no FPU bring-up: -msoft-float in CFLAGS keeps all
// FP math in integer instructions, which the emulator core runs natively).
#define MICROPY_PY_BUILTINS_FLOAT         (1)
#define MICROPY_FLOAT_IMPL                (MICROPY_FLOAT_IMPL_DOUBLE)
#define MICROPY_PY_MATH                   (1)
// bytearray/memoryview: sensor code lives in buffers (write_readinto needs
// a writable dest); cheap, no FPU involved.
#define MICROPY_PY_BUILTINS_BYTEARRAY     (1)
#define MICROPY_PY_BUILTINS_MEMORYVIEW    (1)

// sys module ON, but only for sys.path: frozen imports resolve through the
// ".frozen" sys.path entry (runtime.c appends it when PATH_ARGV_DEFAULTS).
// Everything else sys stays off.
#define MICROPY_PY_SYS                    (1)
#define MICROPY_PY_SYS_MODULES            (0)
#define MICROPY_PY_SYS_EXIT               (0)
#define MICROPY_PY_SYS_ARGV               (0)

// type definitions for the specific machine

typedef long mp_off_t;

// We need to provide a declaration/definition of alloca()
#include <alloca.h>

#define MICROPY_HW_BOARD_NAME "pi3-emu"
#define MICROPY_HW_MCU_NAME "bcm2837"

// 256 KB GC heap in .bss (guest RAM is 4 MB; see bcm2837.ld).
#define MICROPY_HEAP_SIZE      (256 * 1024)

#define MP_STATE_PORT MP_STATE_VM
