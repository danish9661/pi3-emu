# Frozen demo module for the bcm2837 spike (run: import boot).
BOARD = "pi3-emu"


def hello():
    print("hello from frozen", BOARD)
