//! Raw FFI declarations for the subset of dlmdb this crate uses.
//!
//! Hand-written against `lmdb/dlmdb.h` at the pinned upstream commit; the
//! cursor-op ordinals and flag values are copied from that header and are
//! stable LMDB ABI.

#![allow(non_camel_case_types, reason = "C type names keep their C spelling")]

use std::ffi::{c_char, c_int, c_uint, c_void};

#[repr(C)]
pub struct MDB_env {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct MDB_txn {
    _opaque: [u8; 0],
}

#[repr(C)]
pub struct MDB_cursor {
    _opaque: [u8; 0],
}

pub type MDB_dbi = c_uint;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MDB_val {
    pub mv_size: usize,
    pub mv_data: *mut c_void,
}

impl MDB_val {
    pub const EMPTY: MDB_val = MDB_val {
        mv_size: 0,
        mv_data: std::ptr::null_mut(),
    };

    /// View a byte slice as an MDB_val for the duration of a C call.
    pub fn from_slice(bytes: &[u8]) -> MDB_val {
        MDB_val {
            mv_size: bytes.len(),
            mv_data: bytes.as_ptr() as *mut c_void,
        }
    }
}

// ── Environment flags (dlmdb.h) ───────────────────────────────────────────────
pub const MDB_NOSUBDIR: c_uint = 0x4000;
pub const MDB_RDONLY: c_uint = 0x20000;
pub const MDB_NOTLS: c_uint = 0x200000;
pub const MDB_INMEMORY: c_uint = 0x400_0000;

// ── DBI flags ─────────────────────────────────────────────────────────────────
pub const MDB_DUPSORT: c_uint = 0x04;
pub const MDB_COUNTED: c_uint = 0x80;
pub const MDB_PREFIX_COMPRESSION: c_uint = 0x100;
pub const MDB_CREATE: c_uint = 0x40000;

// ── Count-range bound flags (dlmdb extensions) ────────────────────────────────
pub const MDB_COUNT_LOWER_INCL: c_uint = 0x02;
pub const MDB_COUNT_UPPER_INCL: c_uint = 0x04;

// ── Return codes ──────────────────────────────────────────────────────────────
pub const MDB_SUCCESS: c_int = 0;
pub const MDB_KEYEXIST: c_int = -30799;
pub const MDB_NOTFOUND: c_int = -30798;

// ── Cursor operations (ordinal order from dlmdb.h's MDB_cursor_op enum) ──────
pub const MDB_FIRST: c_int = 0;
pub const MDB_FIRST_DUP: c_int = 1;
pub const MDB_GET_BOTH: c_int = 2;
pub const MDB_GET_BOTH_RANGE: c_int = 3;
pub const MDB_GET_CURRENT: c_int = 4;
pub const MDB_LAST: c_int = 6;
pub const MDB_LAST_DUP: c_int = 7;
pub const MDB_NEXT: c_int = 8;
pub const MDB_NEXT_DUP: c_int = 9;
pub const MDB_NEXT_NODUP: c_int = 11;
pub const MDB_PREV: c_int = 12;
pub const MDB_PREV_DUP: c_int = 13;
pub const MDB_PREV_NODUP: c_int = 14;
pub const MDB_SET: c_int = 15;
pub const MDB_SET_KEY: c_int = 16;
pub const MDB_SET_RANGE: c_int = 17;

unsafe extern "C" {
    #[link_name = "dlmdb_env_create"]
    pub fn mdb_env_create(env: *mut *mut MDB_env) -> c_int;
    #[link_name = "dlmdb_env_open"]
    pub fn mdb_env_open(
        env: *mut MDB_env,
        path: *const c_char,
        flags: c_uint,
        mode: c_uint,
    ) -> c_int;
    #[link_name = "dlmdb_env_close"]
    pub fn mdb_env_close(env: *mut MDB_env);
    #[link_name = "dlmdb_env_set_mapsize"]
    pub fn mdb_env_set_mapsize(env: *mut MDB_env, size: usize) -> c_int;
    #[link_name = "dlmdb_env_set_maxdbs"]
    pub fn mdb_env_set_maxdbs(env: *mut MDB_env, dbs: MDB_dbi) -> c_int;
    #[link_name = "dlmdb_env_sync"]
    pub fn mdb_env_sync(env: *mut MDB_env, force: c_int) -> c_int;
    #[link_name = "dlmdb_strerror"]
    pub fn mdb_strerror(err: c_int) -> *const c_char;

    #[link_name = "dlmdb_txn_begin"]
    pub fn mdb_txn_begin(
        env: *mut MDB_env,
        parent: *mut MDB_txn,
        flags: c_uint,
        txn: *mut *mut MDB_txn,
    ) -> c_int;
    #[link_name = "dlmdb_txn_commit"]
    pub fn mdb_txn_commit(txn: *mut MDB_txn) -> c_int;
    #[link_name = "dlmdb_txn_abort"]
    pub fn mdb_txn_abort(txn: *mut MDB_txn);

    #[link_name = "dlmdb_dbi_open"]
    pub fn mdb_dbi_open(
        txn: *mut MDB_txn,
        name: *const c_char,
        flags: c_uint,
        dbi: *mut MDB_dbi,
    ) -> c_int;

    #[link_name = "dlmdb_get"]
    pub fn mdb_get(
        txn: *mut MDB_txn,
        dbi: MDB_dbi,
        key: *const MDB_val,
        data: *mut MDB_val,
    ) -> c_int;
    #[link_name = "dlmdb_put"]
    pub fn mdb_put(
        txn: *mut MDB_txn,
        dbi: MDB_dbi,
        key: *const MDB_val,
        data: *mut MDB_val,
        flags: c_uint,
    ) -> c_int;
    #[link_name = "dlmdb_del"]
    pub fn mdb_del(
        txn: *mut MDB_txn,
        dbi: MDB_dbi,
        key: *const MDB_val,
        data: *const MDB_val,
    ) -> c_int;
    #[link_name = "dlmdb_cmp"]
    pub fn mdb_cmp(txn: *mut MDB_txn, dbi: MDB_dbi, a: *const MDB_val, b: *const MDB_val) -> c_int;

    #[link_name = "dlmdb_cursor_open"]
    pub fn mdb_cursor_open(txn: *mut MDB_txn, dbi: MDB_dbi, cursor: *mut *mut MDB_cursor) -> c_int;
    #[link_name = "dlmdb_cursor_close"]
    pub fn mdb_cursor_close(cursor: *mut MDB_cursor);
    #[link_name = "dlmdb_cursor_get"]
    pub fn mdb_cursor_get(
        cursor: *mut MDB_cursor,
        key: *mut MDB_val,
        data: *mut MDB_val,
        op: c_int,
    ) -> c_int;
    #[link_name = "dlmdb_cursor_count"]
    pub fn mdb_cursor_count(cursor: *mut MDB_cursor, countp: *mut usize) -> c_int;

    // ── dlmdb counted-database extensions ─────────────────────────────────────
    #[link_name = "dlmdb_count_all"]
    pub fn mdb_count_all(txn: *mut MDB_txn, dbi: MDB_dbi, flags: c_uint, out: *mut u64) -> c_int;
    #[link_name = "dlmdb_count_range"]
    pub fn mdb_count_range(
        txn: *mut MDB_txn,
        dbi: MDB_dbi,
        low: *const MDB_val,
        high: *const MDB_val,
        flags: c_uint,
        out: *mut u64,
    ) -> c_int;
    #[link_name = "dlmdb_range_count_keys"]
    pub fn mdb_range_count_keys(
        txn: *mut MDB_txn,
        dbi: MDB_dbi,
        low: *const MDB_val,
        high: *const MDB_val,
        flags: c_uint,
        out: *mut u64,
    ) -> c_int;
    #[link_name = "dlmdb_get_rank"]
    pub fn mdb_get_rank(
        txn: *mut MDB_txn,
        dbi: MDB_dbi,
        rank: u64,
        key: *mut MDB_val,
        data: *mut MDB_val,
    ) -> c_int;
    #[link_name = "dlmdb_get_key_rank"]
    pub fn mdb_get_key_rank(
        txn: *mut MDB_txn,
        dbi: MDB_dbi,
        key: *const MDB_val,
        data: *const MDB_val,
        rank: *mut u64,
    ) -> c_int;
}
