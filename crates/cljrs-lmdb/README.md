# cljrs-lmdb

**Purpose:** the storage substrate for the native datalog store
(datalog-plan.md phase 2): a safe Rust wrapper over **dlmdb**, datalevin's
LMDB fork, vendored under `lmdb/` at the commit pinned in
`lmdb/UPSTREAM_COMMIT` (OpenLDAP Public License 2.8; see `lmdb/LICENSE`).

**Status:** phase 2 complete. Environments, named DBIs, MVCC read
transactions with an in-process-serialized single writer, zero-copy gets,
cursors (incl. dupsort), inclusive range iterators, and the dlmdb
counted-database extensions: O(log n) `count_all`, `count_range` (inclusive
bounds via `MDB_COUNT_*_INCL`), `get_rank`, and `key_rank`. Prefix
compression and the in-memory mode are exposed as flags. Cross-process
concurrent readers are exercised by a re-exec test
(`multiprocess_readers_share_the_environment`).

## File layout

- `lmdb/` — vendored dlmdb C sources (`mdb.c`, `midl.c`, `dlmdb.h`,
  `midl.h`), compiled by `build.rs` with the `cc` crate. Pinned; never
  edited here. Re-vendor deliberately and update `UPSTREAM_COMMIT`.
- `src/sys.rs` — hand-written FFI for the subset in use. Constants are
  copied from `dlmdb.h`; verify against the header when re-vendoring.
- `src/lib.rs` — the safe wrapper: `Env`/`EnvOptions`, `Dbi`, `RoTxn`/
  `RwTxn` (deref for reads), `Cursor`, `RangeIter`, and the counted
  extensions on `RoTxn`.
- `tests/basic.rs` — CRUD, persistence across reopen, sorted/bounded
  ranges, dupsort, counted ranks/counts, prefix compression, snapshot
  isolation, abort, and the cross-process reader.

## Public API sketch

```rust
let env = Env::options().map_size(1 << 30).flags(EnvFlags::NO_TLS).open(dir)?;
let dbi = env.open_dbi("eavt", DbiFlags::CREATE | DbiFlags::COUNTED)?;
let mut txn = env.write_txn()?;   // blocks on the in-process writer lock
txn.put(dbi, key, val)?;
txn.commit()?;
let ro = env.read_txn()?;         // MVCC snapshot
ro.get(dbi, key)?;                // Option<&[u8]>, borrows the map
ro.range(dbi, Some(lo), Some(hi))?; // ascending inclusive iterator
ro.count_range(dbi, Some(lo), Some(hi))?; // O(log n), COUNTED dbis
ro.get_rank(dbi, 42)?;            // nth entry in key order
```

Custom key comparators are deliberately not exposed: the datalog store
encodes order-preserving keys so the default byte comparator is total
(see datalog-plan.md phase 3).
