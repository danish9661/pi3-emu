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
# No compiler-emitted SIMD anywhere: the browser's single-arch core faults
# on NEON (newlib string asm is overridden in string_port.c for the same
# reason). Scalar VFP stays on (the firmware uses doubles natively).
CFLAGS += -fno-tree-vectorize -fno-tree-slp-vectorize
# string_port.c must stay calls-and-scalar: without -fno-builtin GCC turns
# its byte loops back into memcpy/memset calls (infinite recursion).
$(BUILD)/string_port.o: CFLAGS += -fno-builtin
# oofatfs wants its config filename as FFCONF_H (same as extmod.mk).
CFLAGS += -DFFCONF_H=\"lib/oofatfs/ffconf.h\"
# newlib for setjmp/longjmp (MicroPython exceptions) with stub syscalls;
# -nostartfiles keeps newlib's crt0 (and its _start) out: startup.s owns
# the entry, zeroes .bss itself, and calls main.
LDFLAGS = -nostartfiles --specs=nosys.specs -T bcm2837.ld -Wl,-Map=$@.map,--cref -Wl,--gc-sections
LIBS = -lm

SRC_C = \
	main.c \
	uart.c \
	irq.c \
	vfs_port.c \
	fatfs_port.c \
	string_port.c \
	machine.c \
	machine_timer.c \
	machine_i2c.c \
	machine_spi.c \
	machine_uart.c \
	shared/libc/printf.c \
	shared/readline/readline.c \
	shared/runtime/pyexec.c \
	shared/runtime/stdout_helpers.c \
	$(BUILD)/_frozen_mpy.c \

# Extmod sources (os.mount/VfsFat, time module). Compiled from the
# submodule into $(BUILD) (never into the submodule tree); modvfs.c (the
# standalone `vfs` module) is deliberately left out — os.mount covers us.
UPY_EXTMOD_SRC = \
	extmod/modos.c \
	extmod/modtime.c \
	extmod/vfs.c \
	extmod/vfs_blockdev.c \
	extmod/vfs_fat.c \
	extmod/vfs_fat_diskio.c \
	extmod/vfs_fat_file.c \
	extmod/vfs_reader.c \
	lib/oofatfs/ff.c \
	lib/oofatfs/ffunicode.c \
	shared/timeutils/timeutils.c \

# extmod/machine_mem.c (mem8/16/32): explicit rule keeps the object in
# $(BUILD) instead of the submodule tree.
$(BUILD)/extmod_machine_mem.o: ../micropython/extmod/machine_mem.c
	$(ECHO) "CC $<"
	$(Q)mkdir -p $(dir $@)
	$(Q)$(CC) $(CFLAGS) -c -MD -MF $(@:.o=.d) -o $@ $<
	@$(CP) $(@:.o=.d) $(@:.o=.P); \
	  $(SED) -e 's/#.*//' -e 's/^.*:  *//' -e 's/ *\\$$//' \
	      -e '/^$$/ d' -e 's/$$/ :/' < $(@:.o=.d) >> $(@:.o=.P); \
	  $(RM) -f $(@:.o=.d)

# One pattern rule per source dir (make picks the rule whose prereq
# exists); objects land in $(BUILD) as upy_<basename>.o. Depfiles included
# (same recipe as upstream's compile_c): without them these objects never
# rebuild on header/config/qstr-pool changes, baking in stale QSTR numbers
# — proven by os.mount vanishing in M39 (upy_modos.o predated a pool regen).
$(BUILD)/upy_%.o: ../micropython/extmod/%.c
	$(ECHO) "CC $<"
	$(Q)mkdir -p $(dir $@)
	$(Q)$(CC) $(CFLAGS) -c -MD -MF $(@:.o=.d) -o $@ $<
	@$(CP) $(@:.o=.d) $(@:.o=.P); \
	  $(SED) -e 's/#.*//' -e 's/^.*:  *//' -e 's/ *\\$$//' \
	      -e '/^$$/ d' -e 's/$$/ :/' < $(@:.o=.d) >> $(@:.o=.P); \
	  $(RM) -f $(@:.o=.d)
$(BUILD)/upy_%.o: ../micropython/lib/oofatfs/%.c
	$(ECHO) "CC $<"
	$(Q)mkdir -p $(dir $@)
	$(Q)$(CC) $(CFLAGS) -c -MD -MF $(@:.o=.d) -o $@ $<
	@$(CP) $(@:.o=.d) $(@:.o=.P); \
	  $(SED) -e 's/#.*//' -e 's/^.*:  *//' -e 's/ *\\$$//' \
	      -e '/^$$/ d' -e 's/$$/ :/' < $(@:.o=.d) >> $(@:.o=.P); \
	  $(RM) -f $(@:.o=.d)
$(BUILD)/upy_%.o: ../micropython/shared/timeutils/%.c
	$(ECHO) "CC $<"
	$(Q)mkdir -p $(dir $@)
	$(Q)$(CC) $(CFLAGS) -c -MD -MF $(@:.o=.d) -o $@ $<
	@$(CP) $(@:.o=.d) $(@:.o=.P); \
	  $(SED) -e 's/#.*//' -e 's/^.*:  *//' -e 's/ *\\$$//' \
	      -e '/^$$/ d' -e 's/$$/ :/' < $(@:.o=.d) >> $(@:.o=.P); \
	  $(RM) -f $(@:.o=.d)

SRC_S = \
	startup.s \
	vectors.s \

SRC_QSTR += shared/readline/readline.c shared/runtime/pyexec.c machine.c machine_i2c.c machine_spi.c machine_uart.c machine_timer.c irq.c vfs_port.c ../micropython/extmod/machine_mem.c $(addprefix ../micropython/,$(UPY_EXTMOD_SRC))

# extmod/machine_mem.c (mem8/16/32) is covered by the explicit rules above.
OBJ += $(PY_CORE_O)
OBJ += $(addprefix $(BUILD)/, $(SRC_C:.c=.o))
OBJ += $(BUILD)/extmod_machine_mem.o
OBJ += $(addprefix $(BUILD)/upy_, $(notdir $(UPY_EXTMOD_SRC:.c=.o)))
OBJ += $(addprefix $(BUILD)/, $(SRC_S:.s=.o))

all: $(BUILD)/firmware.elf

$(BUILD)/boot.mpy: boot.py
	$(ECHO) "MPY $<"
	$(Q)$(TOP)/mpy-cross/build/mpy-cross -o $@ $<

$(BUILD)/sdcard.mpy: sdcard.py
	$(ECHO) "MPY $<"
	$(Q)$(TOP)/mpy-cross/build/mpy-cross -o $@ $<

$(BUILD)/utime.mpy: utime.py
	$(ECHO) "MPY $<"
	$(Q)$(TOP)/mpy-cross/build/mpy-cross -o $@ $<

$(BUILD)/_frozen_mpy.c: $(BUILD)/boot.mpy $(BUILD)/sdcard.mpy $(BUILD)/utime.mpy $(BUILD)/genhdr/qstrdefs.generated.h
	$(ECHO) "MISC freezing bytecode"
	$(Q)$(TOP)/tools/mpy-tool.py -f -q $(BUILD)/genhdr/qstrdefs.preprocessed.h -mlongint-impl=mpz $(BUILD)/boot.mpy $(BUILD)/sdcard.mpy $(BUILD)/utime.mpy > $@
	# NOTE: keep mpy-tool's "boot.py" entry name as-is: the import machinery
	# appends ".py" before the frozen search (stat_module), so `import boot`
	# exact-matches "boot.py". Renaming the entry to "boot" BREAKS imports.

$(BUILD)/firmware.elf: $(OBJ)
	$(ECHO) "LINK $@"
	$(Q)$(LD) $(LDFLAGS) -o $@ $^ $(LIBS)
	$(Q)$(SIZE) $@

include $(TOP)/py/mkrules.mk
