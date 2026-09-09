# BCM2837 bare-metal MicroPython (out-of-tree port).
#
#   make CROSS_COMPILE=/path/to/aarch64-none-elf- firmware.elf
#
# Needs mpy-cross only for frozen modules (not used by the spike build).
# MicroPython lives at ../micropython (git submodule, upstream master).

include ../micropython/py/mkenv.mk

CROSS_COMPILE ?= aarch64-none-elf-
# Link with the GCC driver (not raw ld): LDFLAGS uses --specs=nosys.specs
# for newlib setjmp/longjmp with stub syscalls.
LD = $(CROSS_COMPILE)gcc
MICROPY_TOP = ../micropython
TOP = $(MICROPY_TOP)

# qstr definitions (must come before including py.mk)
QSTR_DEFS = qstrdefsport.h

# MicroPython feature configurations
MICROPY_ROM_TEXT_COMPRESSION ?= 0

# include py core make definitions
include $(TOP)/py/py.mk

CPUFLAGS = -mcpu=cortex-a53 -mlittle-endian
INC += -I.
INC += -I$(TOP)
INC += -I$(BUILD)

CFLAGS += $(INC) -Wall -Werror -std=c99 $(CPUFLAGS) $(COPT)
CFLAGS += -Os -DNDEBUG -fdata-sections -ffunction-sections
# newlib for setjmp/longjmp (MicroPython exceptions) with stub syscalls;
# -nostartfiles keeps newlib's crt0 (and its _start) out: startup.s owns
# the entry, zeroes .bss itself, and calls main.
LDFLAGS = -nostartfiles --specs=nosys.specs -T bcm2837.ld -Wl,-Map=$@.map,--cref -Wl,--gc-sections
LIBS = -lm

SRC_C = \
	main.c \
	uart.c \
	machine.c \
	machine_i2c.c \
	machine_spi.c \
	machine_uart.c \
	shared/libc/printf.c \
	shared/readline/readline.c \
	shared/runtime/pyexec.c \
	shared/runtime/stdout_helpers.c \
	$(BUILD)/_frozen_mpy.c \

SRC_S = \
	startup.s \

SRC_QSTR += shared/readline/readline.c shared/runtime/pyexec.c machine.c machine_i2c.c machine_spi.c machine_uart.c

OBJ += $(PY_CORE_O)
OBJ += $(addprefix $(BUILD)/, $(SRC_C:.c=.o))
OBJ += $(addprefix $(BUILD)/, $(SRC_S:.s=.o))

all: $(BUILD)/firmware.elf

$(BUILD)/boot.mpy: boot.py
	$(ECHO) "MPY $<"
	$(Q)$(TOP)/mpy-cross/build/mpy-cross -o $@ $<

$(BUILD)/_frozen_mpy.c: $(BUILD)/boot.mpy $(BUILD)/genhdr/qstrdefs.generated.h
	$(ECHO) "MISC freezing bytecode"
	$(Q)$(TOP)/tools/mpy-tool.py -f -q $(BUILD)/genhdr/qstrdefs.preprocessed.h -mlongint-impl=none $< > $@
	# NOTE: keep mpy-tool's "boot.py" entry name as-is: the import machinery
	# appends ".py" before the frozen search (stat_module), so `import boot`
	# exact-matches "boot.py". Renaming the entry to "boot" BREAKS imports.

$(BUILD)/firmware.elf: $(OBJ)
	$(ECHO) "LINK $@"
	$(Q)$(LD) $(LDFLAGS) -o $@ $^ $(LIBS)
	$(Q)$(SIZE) $@

include $(TOP)/py/mkrules.mk
