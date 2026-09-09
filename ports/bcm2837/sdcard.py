# Frozen SDHCI driver + tiny FAT12 reader for the bcm2837 port: the emulated
# SD card holds a 5-sector FAT12 image (see packages/pi3-emu/src/sdhci.js)
# with HELLO.TXT. Read-only (the model has no CMD24 write path).
#
#   import sdcard            # also mounted... no, just lists on import (below)
#   sdcard.ls()              # ['HELLO.TXT']
#   sdcard.read("HELLO.TXT") # b'hello from the SD card\r\n'
#
# (A full VFS mount needs upstream VFS state that master currently lacks —
# see ports/bcm2837/README.md. This module keeps the same SDCard block-device
# shape (readblocks/writeblocks/ioctl) so it can back VfsFat later.)

from machine import mem32

_SD = 0x3F300000
_ARG = _SD + 0x00
_CMD = _SD + 0x04
_RESP0 = _SD + 0x10
_BLOCK = _SD + 0x100
_IRPT = _SD + 0x30
_CMD_COMPLETE = 1


def _cmd(index, arg):
    for _ in range(20000):
        if not mem32[_IRPT] & _CMD_COMPLETE:
            break
    mem32[_ARG] = arg
    mem32[_CMD] = 0x40 | index
    for _ in range(20000):
        if mem32[_IRPT] & _CMD_COMPLETE:
            break
    mem32[_IRPT] = _CMD_COMPLETE
    return mem32[_RESP0]


def _read_sector(n):
    buf = bytearray(512)
    _cmd(17, n)
    for i in range(128):
        w = mem32[_BLOCK + i * 4]
        o = i * 4
        buf[o] = w & 0xFF
        buf[o + 1] = (w >> 8) & 0xFF
        buf[o + 2] = (w >> 16) & 0xFF
        buf[o + 3] = (w >> 24) & 0xFF
    return buf


class SDCard:
    # MicroPython block-device protocol (read-only media).

    def __init__(self):
        _cmd(0, 0)
        _cmd(8, 0x1AA)
        _cmd(55, 0)
        # ACMD41 arg is irrelevant to the model (always reports ready);
        # keep it small-int-safe (no long ints on this build).
        _cmd(41, 0)
        _cmd(2, 0)
        rca = _cmd(3, 0) >> 16
        _cmd(7, rca << 16)

    def readblocks(self, n, buf):
        sec = _read_sector(n)
        for i in range(512):
            buf[i] = sec[i]

    def writeblocks(self, n, buf):
        raise OSError(30)  # read-only media

    def ioctl(self, op, arg):
        if op == 4:  # block count
            return 5
        if op == 5:  # block size
            return 512
        return 0


def _u16(b, o):
    return b[o] | (b[o + 1] << 8)


def _rawname(b):
    # ASCII-only right-trimmed name (bytes.decode is unavailable at this
    # ROM level, so decode manually).
    s = ""
    for i in range(len(b)):
        s = s + chr(b[i])
    while len(s) > 0 and s[len(s) - 1] == " ":
        s = s[:len(s) - 1]
    return s


def _fat12_next(fat, cluster):
    # 12-bit FAT entry for cluster number.
    off = cluster * 3 // 2
    raw = fat[off] | (fat[off + 1] << 8)
    if cluster & 1:
        return raw >> 4
    return raw & 0xFFF


def _layout():
    boot = _read_sector(0)
    reserved = _u16(boot, 14)
    nfats = boot[16]
    spf = _u16(boot, 22)
    nroot = _u16(boot, 17)
    root_sector = reserved + nfats * spf
    root_sectors = (nroot * 32 + 511) // 512
    fat = _read_sector(reserved)
    return root_sector, root_sectors, nroot, fat


def ls():
    root_sector, root_sectors, nroot, _fat = _layout()
    names = []
    for s in range(root_sectors):
        d = _read_sector(root_sector + s)
        for e in range(16):
            o = e * 32
            if d[o] == 0:
                return names
            if d[o] == 0xE5 or d[o + 11] & 0x08:
                continue
            name = _rawname(d[o:o + 8])
            ext = _rawname(d[o + 8:o + 11])
            names.append(name + ("." + ext if ext else ""))
    return names


def read(name):
    root_sector, root_sectors, nroot, fat = _layout()
    data_sector = root_sector + root_sectors
    upper = name.upper()
    for s in range(root_sectors):
        d = _read_sector(root_sector + s)
        for e in range(16):
            o = e * 32
            if d[o] == 0:
                raise OSError(2)  # ENOENT
            if d[o] == 0xE5:
                continue
            nm = _rawname(d[o:o + 8])
            ex = _rawname(d[o + 8:o + 11])
            if nm + ("." + ex if ex else "") != upper:
                continue
            # Size: real FAT12 keeps a u32 at +28; the emulated card splits
            # it (zeros at +28/+29, length at +30/+31 — like the sd guest
            # reads it). Accept either layout.
            size = _u16(d, o + 28)
            if size == 0:
                size = _u16(d, o + 30)
            # NOTE: the emulated card stores the start cluster at dir
            # entry +20 (like the sd guest reads it), not the real-FAT12
            # +26 — model convention, see packages/pi3-emu/src/sdhci.js.
            cluster = _u16(d, o + 20)
            out = bytearray()
            while cluster < 0xFF8 and len(out) < size:
                sec = _read_sector(data_sector + cluster - 2)
                out += sec
                cluster = _fat12_next(fat, cluster)
            return bytes(out[:size])
    raise OSError(2)  # ENOENT
