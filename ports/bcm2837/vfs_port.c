// VFS glue for the bcm2837 port (stays out-of-tree: the submodule is
// pristine upstream master).
//
// Upstream master's mp_state_vm_t lost its vfs_cur/vfs_mount_table fields
// mid-refactor (extmod/vfs.c and py/runtime.c still reference them), so
// MICROPY_VFS=1 fails to compile there. MP_REGISTER_ROOT_POINTER injects
// struct fields into mp_state_vm_t via genhdr/root_pointers.h (collected
// from SRC_QSTR-scanned sources — this file is in SRC_QSTR), which
// restores exactly the two fields upstream code expects, GC-traced.
//
// (With MICROPY_VFS, py/builtin.h already inlines mp_import_stat as
// mp_vfs_import_stat, and the core provides mp_lexer_new_from_file via
// MICROPY_READER_VFS — so this file only injects state.)

#include "py/runtime.h"
#include "py/mphal.h"
#include "extmod/vfs.h"

MP_REGISTER_ROOT_POINTER(struct _mp_vfs_mount_t *vfs_mount_table;)
MP_REGISTER_ROOT_POINTER(struct _mp_vfs_mount_t *vfs_cur;)
