# Frozen demo module for the bcm2837 spike (auto-run at startup, or run:
# import boot).
BOARD = "pi3-emu"


def hello():
    print("hello from frozen", BOARD)


# Static line only: proves auto-run without touching hardware (an SDHCI
# touch with no card attached would data-abort, which no try/except can
# catch). Storage demo: import sdcard, then sdcard.ls().
print("boot: pi3-emu ready (import sdcard for the FAT card)")
