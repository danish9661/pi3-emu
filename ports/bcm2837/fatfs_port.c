// FatFs timestamp hook: no RTC on the emulated board, so stamp a fixed
// date (2026-09-10). Matches the rp2 fatfs_port.c packing.

#include "lib/oofatfs/ff.h"

DWORD get_fattime(void) {
    return ((2026u - 1980u) << 25) | (9u << 21) | (10u << 16);
}
