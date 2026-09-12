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
#define MICROPY_PY_MACHINE_MEMX           (1)
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
// slice syntax (x[a:b]): off at MINIMUM ROM level, but drivers and user
// code need it (the parser rejects slices without it).
#define MICROPY_PY_BUILTINS_SLICE         (1)
#define MICROPY_PY_BUILTINS_SLICE_INDICES (1)

// sys module ON, but only for sys.path: frozen imports resolve through the
// ".frozen" sys.path entry (runtime.c appends it when PATH_ARGV_DEFAULTS).
// Everything else sys stays off.
#define MICROPY_PY_SYS                    (1)
#define MICROPY_PY_SYS_MODULES            (0)
#define MICROPY_PY_SYS_EXIT               (0)
#define MICROPY_PY_SYS_ARGV               (0)

// Full big ints (os.stat timestamps and file sizes go through
// mp_obj_new_int_from_ll, which unconditionally raises under the default
// NONE impl). Small ints stay 63-bit; this only adds the slow path.
#define MICROPY_LONGINT_IMPL            (MICROPY_LONGINT_IMPL_MPZ)
// File I/O + VFS: the FAT12 SD card mounts via os.mount (VfsFat).
// Upstream master's mp_state_vm_t lost its vfs_cur/vfs_mount_table
// fields mid-refactor, so MICROPY_VFS doesn't compile out of the box —
// vfs_port.c injects them via MP_REGISTER_ROOT_POINTER (see it).
#define MICROPY_VFS                 (1)
#define MICROPY_VFS_FAT             (1)
#define MICROPY_READER_VFS          (1)
#define MICROPY_PY_OS               (1)
#define MICROPY_PY_IO               (1)
// time module (sleep/ticks_*) off the system-timer HAL in uart.c; utime
// is a frozen alias (utime.py) for Pico-compatible code.
#define MICROPY_PY_TIME             (1)
// VfsFat file objects need finalisers (flushed/closed on GC).
#define MICROPY_ENABLE_FINALISER    (1)
// Relative paths + getcwd inside the mounted FAT (f_chdir/f_getcwd).
#define MICROPY_FATFS_RPATH         (2)

// type definitions for the specific machine

typedef long mp_off_t;

// We need to provide a declaration/definition of alloca()
#include <alloca.h>

#define MICROPY_HW_BOARD_NAME "pi3-emu"
#define MICROPY_HW_MCU_NAME "bcm2837"

// 256 KB GC heap in .bss (guest RAM is 4 MB; see bcm2837.ld).
#define MICROPY_HEAP_SIZE      (256 * 1024)

#define MP_STATE_PORT MP_STATE_VM
