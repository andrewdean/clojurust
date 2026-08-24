//! Safe wrapper over vendored dlmdb, datalevin's LMDB fork.
//!
//! This is the storage substrate for the native datalog store
//! (datalog-plan.md phase 2). It exposes the LMDB model — one environment
//! per path, named DBIs, MVCC read transactions with a single writer,
//! sorted keys with range scans and dupsort — plus dlmdb's counted-database
//! extensions: O(log n) `count_range`, `count_all`, and rank lookups, which
//! feed the query optimizer's statistics without full scans.
//!
//! Zero-copy reads: `get` and cursor items borrow from the memory map and
//! live as long as the transaction that produced them.

pub mod sys;

use std::ffi::{CStr, CString, c_int, c_uint};
use std::marker::PhantomData;
use std::path::Path;
use std::sync::Mutex;

// ── A minimal bitflags macro (no external dependency) ────────────────────────

macro_rules! bitflags_lite {
    ($(#[$meta:meta])* pub struct $name:ident: $ty:ty { $($(#[$fmeta:meta])* const $flag:ident = $value:expr;)* }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct $name($ty);

        impl $name {
            $($(#[$fmeta])* pub const $flag: $name = $name($value);)*

            /// No flags set.
            #[must_use]
            pub const fn empty() -> Self {
                $name(0)
            }

            /// The raw flag bits.
            #[must_use]
            pub const fn bits(self) -> $ty {
                self.0
            }
        }

        impl std::ops::BitOr for $name {
            type Output = $name;

            fn bitor(self, rhs: $name) -> $name {
                $name(self.0 | rhs.0)
            }
        }
    };
}

/// One LMDB return code surfaced as an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error(pub i32);

impl Error {
    #[must_use]
    pub fn is_not_found(self) -> bool {
        self.0 == sys::MDB_NOTFOUND
    }

    #[must_use]
    pub fn is_key_exist(self) -> bool {
        self.0 == sys::MDB_KEYEXIST
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // SAFETY: mdb_strerror returns a pointer to a static message table
        // for any code, never null.
        let msg = unsafe { CStr::from_ptr(sys::mdb_strerror(self.0)) };
        write!(f, "lmdb error {}: {}", self.0, msg.to_string_lossy())
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn check(rc: c_int) -> Result<()> {
    if rc == sys::MDB_SUCCESS {
        Ok(())
    } else {
        Err(Error(rc))
    }
}

/// SAFETY precondition helper: view an MDB_val the C side just filled as a
/// byte slice bounded by the transaction lifetime the caller chooses.
unsafe fn val_as_slice<'t>(val: &sys::MDB_val) -> &'t [u8] {
    if val.mv_size == 0 {
        &[]
    } else {
        // SAFETY: the caller guarantees `val` was written by a successful
        // dlmdb call inside a still-live transaction; the map pages it
        // points into stay valid until that transaction ends.
        unsafe { std::slice::from_raw_parts(val.mv_data as *const u8, val.mv_size) }
    }
}

// ── Environment ───────────────────────────────────────────────────────────────

bitflags_lite! {
    /// Environment open flags.
    pub struct EnvFlags: c_uint {
        /// The path names a file, not a directory.
        const NO_SUB_DIR = sys::MDB_NOSUBDIR;
        /// Open read-only.
        const READ_ONLY = sys::MDB_RDONLY;
        /// Untie read slots from threads, required when read transactions
        /// move across or outlive OS threads.
        const NO_TLS = sys::MDB_NOTLS;
        /// dlmdb extension: keep the whole database in memory.
        const IN_MEMORY = sys::MDB_INMEMORY;
    }
}

bitflags_lite! {
    /// DBI open flags.
    pub struct DbiFlags: c_uint {
        /// Create the named DBI if absent (write transaction required).
        const CREATE = sys::MDB_CREATE;
        /// Sorted duplicate values per key.
        const DUP_SORT = sys::MDB_DUPSORT;
        /// dlmdb extension: maintain subtree counts, enabling O(log n)
        /// count_range/count_all and the rank lookups.
        const COUNTED = sys::MDB_COUNTED;
        /// dlmdb extension: prefix-compress leaf keys.
        const PREFIX_COMPRESSION = sys::MDB_PREFIX_COMPRESSION;
    }
}

/// Builder for [`Env`].
#[derive(Debug, Clone)]
pub struct EnvOptions {
    map_size: usize,
    max_dbs: u32,
    flags: EnvFlags,
}

impl Default for EnvOptions {
    fn default() -> Self {
        EnvOptions {
            map_size: 1 << 30,
            max_dbs: 32,
            flags: EnvFlags::empty(),
        }
    }
}

impl EnvOptions {
    #[must_use]
    pub fn map_size(mut self, bytes: usize) -> Self {
        self.map_size = bytes;
        self
    }

    #[must_use]
    pub fn max_dbs(mut self, dbs: u32) -> Self {
        self.max_dbs = dbs;
        self
    }

    #[must_use]
    pub fn flags(mut self, flags: EnvFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Open (creating if needed) the environment at `path`, a directory
    /// unless [`EnvFlags::NO_SUB_DIR`] is set.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code when the environment cannot be created,
    /// configured, or opened.
    pub fn open(self, path: &Path) -> Result<Env> {
        let c_path = CString::new(path.as_os_str().as_encoded_bytes())
            .map_err(|_interior_nul| Error(libc_einval()))?;
        let mut raw: *mut sys::MDB_env = std::ptr::null_mut();
        // SAFETY: create → configure → open is the documented init sequence;
        // on any failure the env is closed and never used again.
        unsafe {
            check(sys::mdb_env_create(&raw mut raw))?;
            let configured = check(sys::mdb_env_set_mapsize(raw, self.map_size))
                .and_then(|()| check(sys::mdb_env_set_maxdbs(raw, self.max_dbs)))
                .and_then(|()| {
                    check(sys::mdb_env_open(
                        raw,
                        c_path.as_ptr(),
                        self.flags.bits(),
                        0o664,
                    ))
                });
            if let Err(e) = configured {
                sys::mdb_env_close(raw);
                return Err(e);
            }
        }
        Ok(Env {
            raw,
            write_lock: Mutex::new(()),
        })
    }
}

fn libc_einval() -> i32 {
    22
}

/// One open dlmdb environment.
///
/// Thread-safe; the single-writer rule is enforced in-process by a mutex
/// (a second `write_txn` blocks until the first commits or aborts) and
/// across processes by LMDB's own environment lock.
#[derive(Debug)]
pub struct Env {
    raw: *mut sys::MDB_env,
    write_lock: Mutex<()>,
}

// SAFETY: MDB_env is documented thread-safe (all mutation goes through
// transactions); the raw pointer is only invalidated by Drop, which takes
// ownership.
unsafe impl Send for Env {}
// SAFETY: as above; concurrent readers are the LMDB design point.
unsafe impl Sync for Env {}

impl Drop for Env {
    fn drop(&mut self) {
        // SAFETY: raw was returned by mdb_env_create and opened; transactions
        // borrow the Env, so none outlive it.
        unsafe { sys::mdb_env_close(self.raw) };
    }
}

impl Env {
    #[must_use]
    pub fn options() -> EnvOptions {
        EnvOptions::default()
    }

    /// Open a named DBI, creating it when `flags` contains
    /// [`DbiFlags::CREATE`]. Runs its own short write transaction.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code when the DBI cannot be opened or created.
    pub fn open_dbi(&self, name: &str, flags: DbiFlags) -> Result<Dbi> {
        let txn = self.write_txn()?;
        let c_name = CString::new(name).map_err(|_interior_nul| Error(libc_einval()))?;
        let mut dbi: sys::MDB_dbi = 0;
        // SAFETY: txn is a live write transaction on this env; the name
        // pointer outlives the call.
        check(unsafe { sys::mdb_dbi_open(txn.raw, c_name.as_ptr(), flags.bits(), &raw mut dbi) })?;
        txn.commit()?;
        Ok(Dbi(dbi))
    }

    /// Open an existing named DBI without writing, for read-only
    /// environments. The opening read transaction is committed so the
    /// handle becomes valid environment-wide.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code when the DBI does not exist or cannot open.
    pub fn open_dbi_read_only(&self, name: &str) -> Result<Dbi> {
        let c_name = CString::new(name).map_err(|_interior_nul| Error(libc_einval()))?;
        let mut txn = self.read_txn()?;
        let mut dbi: sys::MDB_dbi = 0;
        // SAFETY: txn is a live read transaction on this env.
        let opened = check(unsafe { sys::mdb_dbi_open(txn.raw, c_name.as_ptr(), 0, &raw mut dbi) });
        let raw = txn.raw;
        txn.raw = std::ptr::null_mut();
        // SAFETY: raw was live and is now owned solely by this call; the
        // nulled field keeps Drop from double-aborting.
        let committed = check(unsafe { sys::mdb_txn_commit(raw) });
        opened.and(committed)?;
        Ok(Dbi(dbi))
    }

    /// Begin a read transaction (an MVCC snapshot).
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code when the reader slot cannot be acquired.
    pub fn read_txn(&self) -> Result<RoTxn<'_>> {
        let mut raw: *mut sys::MDB_txn = std::ptr::null_mut();
        // SAFETY: env is open; RDONLY txns need no lock coordination here.
        check(unsafe {
            sys::mdb_txn_begin(
                self.raw,
                std::ptr::null_mut(),
                sys::MDB_RDONLY,
                &raw mut raw,
            )
        })?;
        Ok(RoTxn {
            raw,
            _env: PhantomData,
        })
    }

    /// Begin the write transaction, waiting for any in-process writer first.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code when the transaction cannot begin.
    pub fn write_txn(&self) -> Result<RwTxn<'_>> {
        let guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut raw: *mut sys::MDB_txn = std::ptr::null_mut();
        // SAFETY: env is open; the in-process mutex means we never issue two
        // concurrent write-txn begins from this Env value (LMDB would block
        // on its own mutex otherwise, which is deadlock-prone with Rust
        // borrows around it).
        check(unsafe { sys::mdb_txn_begin(self.raw, std::ptr::null_mut(), 0, &raw mut raw) })?;
        Ok(RwTxn {
            ro: RoTxn {
                raw,
                _env: PhantomData,
            },
            _guard: guard,
        })
    }
}

/// A named database handle. Copyable; valid for the life of the [`Env`].
#[derive(Debug, Clone, Copy)]
pub struct Dbi(sys::MDB_dbi);

// ── Transactions ──────────────────────────────────────────────────────────────

/// A read-only transaction: a consistent snapshot of the environment.
#[derive(Debug)]
pub struct RoTxn<'e> {
    raw: *mut sys::MDB_txn,
    _env: PhantomData<&'e Env>,
}

impl Drop for RoTxn<'_> {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            // SAFETY: raw is live (commit/abort null it before returning).
            unsafe { sys::mdb_txn_abort(self.raw) };
        }
    }
}

impl<'e> RoTxn<'e> {
    /// Look up `key`, returning a map-backed slice valid for this txn.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code on lookup failure other than absence.
    pub fn get<'t>(&'t self, dbi: Dbi, key: &[u8]) -> Result<Option<&'t [u8]>> {
        let k = sys::MDB_val::from_slice(key);
        let mut v = sys::MDB_val::EMPTY;
        // SAFETY: txn live, dbi from this env, key pointer valid for the call.
        let rc = unsafe { sys::mdb_get(self.raw, dbi.0, &raw const k, &raw mut v) };
        match rc {
            sys::MDB_SUCCESS => {
                // SAFETY: v was filled by a successful get inside this txn.
                Ok(Some(unsafe { val_as_slice(&v) }))
            }
            sys::MDB_NOTFOUND => Ok(None),
            other => Err(Error(other)),
        }
    }

    /// Compare two values with `dbi`'s key comparator.
    #[must_use]
    pub fn cmp(&self, dbi: Dbi, a: &[u8], b: &[u8]) -> std::cmp::Ordering {
        let av = sys::MDB_val::from_slice(a);
        let bv = sys::MDB_val::from_slice(b);
        // SAFETY: txn live, dbi from this env, both pointers valid for the call.
        let r = unsafe { sys::mdb_cmp(self.raw, dbi.0, &raw const av, &raw const bv) };
        r.cmp(&0)
    }

    /// Open a cursor over `dbi` in this transaction.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code when the cursor cannot be opened.
    pub fn cursor(&self, dbi: Dbi) -> Result<Cursor<'_>> {
        let mut raw: *mut sys::MDB_cursor = std::ptr::null_mut();
        // SAFETY: txn live, dbi from this env.
        check(unsafe { sys::mdb_cursor_open(self.raw, dbi.0, &raw mut raw) })?;
        Ok(Cursor {
            raw,
            _txn: PhantomData,
        })
    }

    /// Ascending iterator over `[low, high]` (inclusive bounds; `None` is
    /// open). Bounds are compared with `dbi`'s comparator.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code when the cursor cannot be opened.
    pub fn range<'t>(
        &'t self,
        dbi: Dbi,
        low: Option<&'t [u8]>,
        high: Option<&'t [u8]>,
    ) -> Result<RangeIter<'t>> {
        Ok(RangeIter {
            cursor: self.cursor(dbi)?,
            txn: self,
            dbi,
            low: low.map(<[u8]>::to_vec),
            high: high.map(<[u8]>::to_vec),
            started: false,
            done: false,
        })
    }

    /// dlmdb: total entries in a [`DbiFlags::COUNTED`] database, O(log n).
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code (including for non-counted DBIs).
    pub fn count_all(&self, dbi: Dbi) -> Result<u64> {
        let mut out = 0_u64;
        // SAFETY: txn live, dbi from this env.
        check(unsafe { sys::mdb_count_all(self.raw, dbi.0, 0, &raw mut out) })?;
        Ok(out)
    }

    /// dlmdb: entries within `[low, high]` inclusive in a counted database,
    /// O(log n). Open bounds count from either end.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code (including for non-counted DBIs).
    pub fn count_range(&self, dbi: Dbi, low: Option<&[u8]>, high: Option<&[u8]>) -> Result<u64> {
        let lv = low.map(sys::MDB_val::from_slice);
        let hv = high.map(sys::MDB_val::from_slice);
        let lp = lv
            .as_ref()
            .map_or(std::ptr::null(), |v| v as *const sys::MDB_val);
        let hp = hv
            .as_ref()
            .map_or(std::ptr::null(), |v| v as *const sys::MDB_val);
        let mut out = 0_u64;
        let flags = sys::MDB_COUNT_LOWER_INCL | sys::MDB_COUNT_UPPER_INCL;
        // SAFETY: txn live; bound pointers valid for the call or null.
        check(unsafe { sys::mdb_count_range(self.raw, dbi.0, lp, hp, flags, &raw mut out) })?;
        Ok(out)
    }

    /// dlmdb: entries strictly below `key` in a counted database,
    /// O(log n). This is the rank the first entry >= `key` would have,
    /// whether or not `key` itself is present.
    ///
    /// Built from present-key operations (seek then rank) because
    /// `mdb_count_range` bound handling for absent keys is subtle.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code (including for non-counted DBIs).
    pub fn count_below(&self, dbi: Dbi, key: &[u8]) -> Result<u64> {
        let mut cursor = self.cursor(dbi)?;
        match cursor.set_range(key)? {
            None => self.count_all(dbi),
            Some((found, _)) => {
                let found = found.to_vec();
                drop(cursor);
                self.key_rank(dbi, &found)?.ok_or(Error(sys::MDB_NOTFOUND))
            }
        }
    }

    /// dlmdb: the entry at `rank` (0-based, in key order) in a counted
    /// database, O(log n).
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code; rank past the end reports not-found.
    pub fn get_rank(&self, dbi: Dbi, rank: u64) -> Result<Option<(&[u8], &[u8])>> {
        let mut k = sys::MDB_val::EMPTY;
        let mut v = sys::MDB_val::EMPTY;
        // SAFETY: txn live, dbi from this env.
        let rc = unsafe { sys::mdb_get_rank(self.raw, dbi.0, rank, &raw mut k, &raw mut v) };
        match rc {
            // SAFETY: both vals were filled by a successful call in this txn.
            sys::MDB_SUCCESS => Ok(Some(unsafe { (val_as_slice(&k), val_as_slice(&v)) })),
            sys::MDB_NOTFOUND => Ok(None),
            other => Err(Error(other)),
        }
    }

    /// dlmdb: the rank of `key` in a counted database, O(log n).
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code; an absent key reports not-found as `None`.
    pub fn key_rank(&self, dbi: Dbi, key: &[u8]) -> Result<Option<u64>> {
        let k = sys::MDB_val::from_slice(key);
        let mut rank = 0_u64;
        // SAFETY: txn live; key pointer valid for the call.
        let rc = unsafe {
            sys::mdb_get_key_rank(
                self.raw,
                dbi.0,
                &raw const k,
                std::ptr::null(),
                &raw mut rank,
            )
        };
        match rc {
            sys::MDB_SUCCESS => Ok(Some(rank)),
            sys::MDB_NOTFOUND => Ok(None),
            other => Err(Error(other)),
        }
    }
}

/// The write transaction. Derefs to [`RoTxn`] for reads.
#[derive(Debug)]
pub struct RwTxn<'e> {
    ro: RoTxn<'e>,
    _guard: std::sync::MutexGuard<'e, ()>,
}

impl<'e> std::ops::Deref for RwTxn<'e> {
    type Target = RoTxn<'e>;

    fn deref(&self) -> &RoTxn<'e> {
        &self.ro
    }
}

impl RwTxn<'_> {
    /// Insert or replace `key` → `value`.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code (map full, incompatible flags, ...).
    pub fn put(&mut self, dbi: Dbi, key: &[u8], value: &[u8]) -> Result<()> {
        let k = sys::MDB_val::from_slice(key);
        let mut v = sys::MDB_val::from_slice(value);
        // SAFETY: write txn live; pointers valid for the call.
        check(unsafe { sys::mdb_put(self.ro.raw, dbi.0, &raw const k, &raw mut v, 0) })
    }

    /// Delete `key` (all duplicates), or one specific `key`/`value` pair in
    /// a dupsort database. Returns whether anything was deleted.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code on failures other than absence.
    pub fn del(&mut self, dbi: Dbi, key: &[u8], value: Option<&[u8]>) -> Result<bool> {
        let k = sys::MDB_val::from_slice(key);
        let vv = value.map(sys::MDB_val::from_slice);
        let vp = vv
            .as_ref()
            .map_or(std::ptr::null(), |v| v as *const sys::MDB_val);
        // SAFETY: write txn live; pointers valid for the call or null.
        let rc = unsafe { sys::mdb_del(self.ro.raw, dbi.0, &raw const k, vp) };
        match rc {
            sys::MDB_SUCCESS => Ok(true),
            sys::MDB_NOTFOUND => Ok(false),
            other => Err(Error(other)),
        }
    }

    /// Commit the transaction.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code when the commit fails (the txn is freed
    /// either way).
    pub fn commit(mut self) -> Result<()> {
        let raw = self.ro.raw;
        self.ro.raw = std::ptr::null_mut();
        // SAFETY: raw was live and is now owned solely by this call; the
        // nulled field keeps Drop from double-freeing.
        check(unsafe { sys::mdb_txn_commit(raw) })
    }

    /// Abort the transaction, discarding its writes.
    pub fn abort(mut self) {
        let raw = self.ro.raw;
        self.ro.raw = std::ptr::null_mut();
        // SAFETY: as in commit.
        unsafe { sys::mdb_txn_abort(raw) };
    }
}

// ── Cursors ───────────────────────────────────────────────────────────────────

/// A cursor over one DBI within a transaction.
#[derive(Debug)]
pub struct Cursor<'t> {
    raw: *mut sys::MDB_cursor,
    _txn: PhantomData<&'t ()>,
}

impl Drop for Cursor<'_> {
    fn drop(&mut self) {
        // SAFETY: raw came from mdb_cursor_open and is closed exactly once.
        unsafe { sys::mdb_cursor_close(self.raw) };
    }
}

type Entry<'t> = (&'t [u8], &'t [u8]);

impl<'t> Cursor<'t> {
    fn op(&mut self, key: Option<&[u8]>, op: c_int) -> Result<Option<Entry<'t>>> {
        let mut k = key.map_or(sys::MDB_val::EMPTY, sys::MDB_val::from_slice);
        let mut v = sys::MDB_val::EMPTY;
        // SAFETY: cursor live; key pointer (when set) valid for the call.
        let rc = unsafe { sys::mdb_cursor_get(self.raw, &raw mut k, &raw mut v, op) };
        match rc {
            // SAFETY: both vals were filled by a successful cursor op whose
            // transaction outlives 't.
            sys::MDB_SUCCESS => Ok(Some(unsafe { (val_as_slice(&k), val_as_slice(&v)) })),
            sys::MDB_NOTFOUND => Ok(None),
            other => Err(Error(other)),
        }
    }

    /// Position at the first entry.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code on cursor failure.
    pub fn first(&mut self) -> Result<Option<Entry<'t>>> {
        self.op(None, sys::MDB_FIRST)
    }

    /// Position at the last entry.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code on cursor failure.
    pub fn last(&mut self) -> Result<Option<Entry<'t>>> {
        self.op(None, sys::MDB_LAST)
    }

    /// Advance to the next entry (dupsort: next value, then next key).
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code on cursor failure.
    #[expect(
        clippy::should_implement_trait,
        reason = "LMDB cursor vocabulary; a fallible positioned cursor is not an Iterator"
    )]
    pub fn next(&mut self) -> Result<Option<Entry<'t>>> {
        self.op(None, sys::MDB_NEXT)
    }

    /// Step back to the previous entry.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code on cursor failure.
    pub fn prev(&mut self) -> Result<Option<Entry<'t>>> {
        self.op(None, sys::MDB_PREV)
    }

    /// Position at the first entry with key >= `key`.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code on cursor failure.
    pub fn set_range(&mut self, key: &[u8]) -> Result<Option<Entry<'t>>> {
        self.op(Some(key), sys::MDB_SET_RANGE)
    }

    /// Position at `key` exactly.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code on cursor failure.
    pub fn set(&mut self, key: &[u8]) -> Result<Option<Entry<'t>>> {
        self.op(Some(key), sys::MDB_SET_KEY)
    }

    /// Dupsort: advance among the current key's values only.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code on cursor failure.
    pub fn next_dup(&mut self) -> Result<Option<Entry<'t>>> {
        self.op(None, sys::MDB_NEXT_DUP)
    }

    /// Dupsort: jump to the first value of the next key.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code on cursor failure.
    pub fn next_nodup(&mut self) -> Result<Option<Entry<'t>>> {
        self.op(None, sys::MDB_NEXT_NODUP)
    }

    /// Dupsort: number of values for the current key.
    ///
    /// # Errors
    ///
    /// Returns the dlmdb code on cursor failure.
    pub fn dup_count(&mut self) -> Result<u64> {
        let mut count = 0_usize;
        // SAFETY: cursor live and positioned.
        check(unsafe { sys::mdb_cursor_count(self.raw, &raw mut count) })?;
        Ok(count as u64)
    }
}

/// Ascending inclusive-range iterator produced by [`RoTxn::range`].
pub struct RangeIter<'t> {
    cursor: Cursor<'t>,
    txn: &'t RoTxn<'t>,
    dbi: Dbi,
    low: Option<Vec<u8>>,
    high: Option<Vec<u8>>,
    started: bool,
    done: bool,
}

impl<'t> Iterator for RangeIter<'t> {
    type Item = Result<Entry<'t>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let step = if self.started {
            self.cursor.next()
        } else {
            self.started = true;
            match &self.low {
                Some(low) => self.cursor.set_range(low),
                None => self.cursor.first(),
            }
        };
        match step {
            Err(e) => {
                self.done = true;
                Some(Err(e))
            }
            Ok(None) => {
                self.done = true;
                None
            }
            Ok(Some((k, v))) => {
                if let Some(high) = &self.high
                    && self.txn.cmp(self.dbi, k, high) == std::cmp::Ordering::Greater
                {
                    self.done = true;
                    return None;
                }
                Some(Ok((k, v)))
            }
        }
    }
}
