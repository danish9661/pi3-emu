# Frozen SDHCI driver + tiny FAT12 file access for the bcm2837 port: the
# emulated SD card holds a 5-sector FAT12 image (see
# packages/pi3-emu/src/sdhci.js) with HELLO.TXT. Reads and writes
# (the model implements CMD24 single-block writes).
#
#   import sdcard            # also mounted... no, just lists on import (below)
#   sdcard.ls()              # ['HELLO.TXT']
#   sdcard.read("HELLO.TXT") # b'hello from the SD card\r\n'
#   sdcard.write("HELLO.TXT", b"new bytes")  # create or overwrite (the
#                                            # chain grows/shrinks as needed)
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


def _write_sector(n, buf):
    for i in range(128):
        o = i * 4
        mem32[_BLOCK + i * 4] = (buf[o] | (buf[o + 1] << 8) |
                                 (buf[o + 2] << 16) | (buf[o + 3] << 24))
    _cmd(24, n)


class SDCard:
    # MicroPython block-device protocol.

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
        sec = bytearray(512)
        for i in range(512):
            sec[i] = buf[i]
        _write_sector(n, sec)

    def ioctl(self, op, arg):
        if op == 4:  # block count
            return 5
        if op == 5:  # block size
            return 512
        return 0


def _u16(b, o):
    return b[o] | (b[o + 1] << 8)


def _u32(b, o):
    return b[o] | (b[o + 1] << 8) | (b[o + 2] << 16) | (b[o + 3] << 24)


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


def _fat12_set(fat, cluster, value):
    # 12-bit FAT entry write (mirror of _fat12_next; even/odd nibbles).
    # PROOF: set-then-next round-trips (see test/upython-sd.mjs create).
    off = cluster * 3 // 2
    raw = fat[off] | (fat[off + 1] << 8)
    if cluster & 1:
        raw = (raw & 0x000F) | ((value & 0xFFF) << 4)
    else:
        raw = (raw & 0xF000) | (value & 0xFFF)
    fat[off] = raw & 0xFF
    fat[off + 1] = (raw >> 8) & 0xFF


# Model growth cap (writeSector in sdhci.js): sectors 0..31 exist.
_MAX_SECTOR = 32
# Fresh-file stamp (same as the card image): 2026-09-10 12:00.
_STAMP_TIME = 0x6000
_STAMP_DATE = 0x5D2A


def _layout():
    boot = _read_sector(0)
    reserved = _u16(boot, 14)
    nfats = boot[16]
    spf = _u16(boot, 22)
    nroot = _u16(boot, 17)
    root_sector = reserved + nfats * spf
    root_sectors = (nroot * 32 + 511) // 512
    fat = _read_sector(reserved)
    return reserved, spf, root_sector, root_sectors, nroot, fat


def _chain(fat, cluster):
    out = []
    c = cluster
    while 2 <= c < 0xFF8 and len(out) < 64:
        out.append(c)
        c = _fat12_next(fat, c)
    return out


def _free_clusters(fat, data_sector, need):
    found = []
    c = 2
    while len(found) < need and c < 512 and data_sector + c - 2 < _MAX_SECTOR:
        if _fat12_next(fat, c) == 0:
            found.append(c)
        c += 1
    return found


def _encname(name):
    # "NAME.EXT" -> (8-char base, 3-char ext), uppercased, space-padded.
    parts = name.upper().split(".")
    if len(parts) > 2 or not parts[0] or len(parts[0]) > 8 or len(parts[-1]) > 3:
        raise ValueError("bad name")
    base = parts[0]
    ext = parts[1] if len(parts) == 2 else ""
    return base, ext


def ls():
    _reserved, _spf, root_sector, root_sectors, nroot, _fat = _layout()
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
    _reserved, _spf, root_sector, root_sectors, nroot, fat = _layout()
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
            # Real FAT12 entry: start cluster u16 at +26, size u32 at +28
            # (with fallbacks to the pre-VFS split layout the old image
            # used: cluster at +20, length at +30/+31).
            size = _u32(d, o + 28)
            if size == 0:
                size = _u16(d, o + 30)
            cluster = _u16(d, o + 26)
            if cluster == 0:
                cluster = _u16(d, o + 20)
            out = bytearray()
            while cluster < 0xFF8 and len(out) < size:
                sec = _read_sector(data_sector + cluster - 2)
                out += sec
                cluster = _fat12_next(fat, cluster)
            return bytes(out[:size])
    raise OSError(2)  # ENOENT


def write(name, data):
    # Create or overwrite a file, growing/shrinking its cluster chain as
    # needed (free clusters are scanned, the tail is freed on shrink, both
    # FAT copies are written back). Returns the byte count. Root capacity
    # is fixed (16 entries); image growth stops at _MAX_SECTOR sectors.
    # NOTE: raw writes bypass a live VfsFat mount's sector cache — umount
    # and remount before reading back through /sd (same as real systems).
    # Stronger rule: never interleave raw writes with VFS *writes* while
    # mounted — FatFs writes sectors back from its stale cache and will
    # silently clobber raw-written entries. umount first, then write raw.
    reserved, spf, root_sector, root_sectors, nroot, fat = _layout()
    data_sector = root_sector + root_sectors
    upper = name.upper()
    found = None
    free = None
    d = None
    for s in range(root_sectors):
        d = _read_sector(root_sector + s)
        for e in range(16):
            o = e * 32
            if d[o] == 0 or d[o] == 0xE5:
                if free is None:
                    free = (s, o)
                if d[o] == 0:
                    break
                continue
            if d[o + 11] & 0x08:
                continue
            nm = _rawname(d[o:o + 8])
            ex = _rawname(d[o + 8:o + 11])
            if nm + ("." + ex if ex else "") == upper:
                found = (s, o)
                break
        if found is not None:
            break
    if found is None:
        if free is None:
            raise OSError(28)  # ENOSPC: root directory full
        s, o = free
        d = _read_sector(root_sector + s)
        base, ext = _encname(name)
        for i in range(8):
            d[o + i] = ord(base[i]) if i < len(base) else 32
        for i in range(3):
            d[o + 8 + i] = ord(ext[i]) if i < len(ext) else 32
        d[o + 11] = 0x20
        d[o + 22] = _STAMP_TIME & 0xFF
        d[o + 23] = (_STAMP_TIME >> 8) & 0xFF
        d[o + 24] = _STAMP_DATE & 0xFF
        d[o + 25] = (_STAMP_DATE >> 8) & 0xFF
        d[o + 26] = 0
        d[o + 27] = 0
        cluster = 0
    else:
        s, o = found
        d = _read_sector(root_sector + s)
        cluster = _u16(d, o + 26)
        if cluster == 0:
            cluster = _u16(d, o + 20)
    chain = _chain(fat, cluster) if cluster >= 2 else []
    need = (len(data) + 511) // 512
    if need > len(chain):
        extra = _free_clusters(fat, data_sector, need - len(chain))
        if len(extra) < need - len(chain):
            raise OSError(28)  # ENOSPC: no free clusters
        if chain:
            _fat12_set(fat, chain[len(chain) - 1], extra[0])
        for i in range(len(extra)):
            nxt = extra[i + 1] if i + 1 < len(extra) else 0xFFF
            _fat12_set(fat, extra[i], nxt)
            sec = bytearray(512)
            _write_sector(data_sector + extra[i] - 2, sec)
        chain = chain + extra
        cluster = chain[0]
    elif need < len(chain):
        for c in chain[need:]:
            _fat12_set(fat, c, 0)
        chain = chain[:need]
        if need == 0:
            cluster = 0
        else:
            _fat12_set(fat, chain[need - 1], 0xFFF)
            cluster = chain[0]
    for i in range(len(chain)):
        sec = bytearray(512)
        chunk = data[i * 512:(i + 1) * 512]
        for j in range(len(chunk)):
            sec[j] = chunk[j]
        _write_sector(data_sector + chain[i] - 2, sec)
    # Real FAT12 entry: start cluster u16 at +26, size u32 at +28.
    n = len(data)
    d[o + 26] = cluster & 0xFF
    d[o + 27] = (cluster >> 8) & 0xFF
    d[o + 28] = n & 0xFF
    d[o + 29] = (n >> 8) & 0xFF
    d[o + 30] = (n >> 16) & 0xFF
    d[o + 31] = (n >> 24) & 0xFF
    _write_sector(root_sector + s, d)
    _write_sector(reserved, fat)
    for k in range(1, spf):
        _write_sector(reserved + k, fat)
    return n
