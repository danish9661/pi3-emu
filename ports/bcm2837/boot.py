# Frozen demo module for the bcm2837 spike (auto-run at startup, or run:
# import boot).
BOARD = "pi3-emu"
# SD-card presence flag: host extension word in the always-mapped mailbox
# window (see Pi3Emulator.SD_PRESENT). Reading it can never data-abort,
# unlike touching the SDHCI window with no card mapped.
_SD_PRESENT = 0x3F00BFF0


def hello():
    print("hello from frozen", BOARD)


print("boot: pi3-emu ready (import sdcard for the FAT card)")
# Auto-mount /sd when the host maps a card. Silent unless the mount works:
# startup must always reach the REPL (a bad card raises OSError here, and
# manual SDHCI touches with no card attached would data-abort — which no
# try/except can catch — so never touch SDHCI unless the flag is set).
try:
    from machine import mem32
    if mem32[_SD_PRESENT]:
        import os
        import sdcard
        import sys
        os.mount(os.VfsFat(sdcard.SDCard()), "/sd")
        sys.path.append("/sd")
        print("boot: /sd mounted:", os.listdir("/sd"))
except OSError:
    pass
