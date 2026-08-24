# cljrs-datalog-store

**Purpose:** the datalog triple store over `cljrs-lmdb`
(datalog-plan.md phase 3): the Rust storage half beneath the ported
Clojure query engine. Replaces datalevin's `bits.clj`/`storage.clj`
JVM layer with native indexes and serde.

**Status:** phase 3 core complete. Three counted indexes with
order-preserving composite keys — `eav` `[e:8][aid:4][value]`, `ave`
`[aid:4][value][e:8]` (prefix-compressed), `vae` `[v:8][aid:4][e:8]`
for refs — plus `schema` (attr → aid + flags), `meta` (counters), and
content-addressed `giants`/`giant-ids` for oversized values. Search
covers the full e/a/v pattern case tree with index selection;
`count`/`cardinality` answer in O(log n) via rank differences;
`sample_ave` rank-strides an attribute's range for optimizer value
distributions. Cardinality-one asserts replace; retracts clean every
index. Upserts, tempids, and tx reports live above the store.

## File layout

- `src/codec.rs` — the order-preserving value codec: tag byte per type,
  sign-flipped big-endian longs, IEEE-total-order doubles, escaped and
  zero-terminated strings/bytes (prefix-free, so values parse and sort
  in the middle of `ave` keys). A seeded randomized test proves byte
  order equals value order. Giants: `[tag][64-byte prefix][id:8]` in
  keys, full value in the giants DBI keyed by id, ids deduplicated by
  SHA-256 so equal values share one id and exact-match lookups work.
- `src/lib.rs` — `Store` (open/open_with_flags incl. dlmdb in-memory),
  `set_attr`/`AttrProps`, `transact(&[Op])`, `search`, `count`,
  `cardinality`, `sample_ave`, `max_eid`.
- `tests/store.rs` — the pattern case tree, cardinality semantics,
  multi-index retraction, counts and rank sampling, giant roundtrips,
  reopen persistence, cross-type value ordering.

## Documented divergences

- Giant values sort by 64-byte prefix then insertion id, not full value
  order; range scans across giants are approximate (residual predicates
  above the store make them exact). Datalevin's giant handling is
  equivalent in spirit.
- Retracted giants are not garbage-collected yet.
- Cross-type ordering is by type tag (datalog's class ordering), so
  heterogeneous attribute ranges group by type.
