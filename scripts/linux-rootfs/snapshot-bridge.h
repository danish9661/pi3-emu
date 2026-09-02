#ifndef HW_MISC_SNAPSHOT_BRIDGE_H
#define HW_MISC_SNAPSHOT_BRIDGE_H
#include "hw/sysbus.h"
#define TYPE_SNAPSHOT_BRIDGE "snapshot-bridge"
#define SNAPSHOT_BRIDGE(obj) OBJECT_CHECK(SnapshotBridgeState, (obj), TYPE_SNAPSHOT_BRIDGE)
typedef struct SnapshotBridgeState {
    SysBusDevice parent_obj;
    MemoryRegion iomem;
    uint32_t ctrl;   // 0x00: bit0 SAVE, bit1 LOAD, bit2 BUSY
    uint32_t status; // 0x04: last result
} SnapshotBridgeState;
#endif
