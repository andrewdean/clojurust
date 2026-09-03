# Dataframes with cljrs.polars

`cljrs.polars` binds the [Polars](https://pola.rs) dataframe engine as a
native package: lazy query plans, Parquet/CSV/IPC IO, joins (including
as-of), group-bys, window functions, and a SQL context — driven from
Clojure, with columns marshalling into the runtime's `double-array` /
`long-array` values so results compose with `cljrs.num` kernels and plain
Clojure code.

## Setup

The package loads through the `:rust/load :dylib` mechanism. In your
project's `cljrs.edn`:

```edn
{:deps {cljrs.polars {:git/url "https://github.com/andrewdean/cljrs-polars"
                      :git/sha "<commit>"
                      :rust/init "cljrs_polars::cljrs_init"
                      :rust/load :dylib}}}
```

The first `require` fetches the pinned tree, builds a wrapper cdylib
(minutes; cached afterwards), and registers the namespace. Set
`CLJRS_WORKSPACE_ROOT` to the clojurust checkout your `cljrs` binary was
built from.

```clojure
(require '[cljrs.polars :as pl])
```

## Scanning and inspecting

Frames are opaque handles. `scan-*` is lazy (nothing reads until collect);
globs work, including Hive-partitioned directories:

```clojure
(def nav (pl/scan-parquet "out/parquet/nav_daily/**/*.parquet"))

(pl/schema nav)
;; => [["as_of_date" "date"] ["fund_id" "i64"] ["gross_nav" "decimal[20,4]"] ...]

(def df (pl/collect nav))
(pl/shape df)        ;; => [3888 8]
(pl/head df 2)       ;; => vector of keyword-keyed row maps
(pl/explain nav)     ;; the optimizer's plan, as a string
```

## Columns are primitive arrays

`column` marshals into the runtime's unboxed arrays — floats (and
decimals) as `double-array`, integers and dates (epoch days) as
`long-array`, strings/booleans as vectors with `nil` for null:

```clojure
(reduce max (pl/column df "gross_nav"))
(vec (pl/fill-null ...))                      ; or feed them to cljrs.num
```

The reverse direction is `from-arrays` (pair form preserves column order):

```clojure
(pl/from-arrays [["day" [:date (long-array [18262 18263])]]
                 ["px"  (double-array [101.25 101.75])]])
```

## The lazy relational core

Ops compose with `->`; eager frames lift into the pipeline automatically:

```clojure
(-> (pl/scan-parquet "out/parquet/trade/**/*.parquet")
    (pl/filter (pl/= (pl/col "side") "BUY"))
    (pl/with-columns
     [(pl/alias (pl/* (pl/cast (pl/col "quantity") :f64)
                      (pl/cast (pl/col "price") :f64))
                "notional")])
    (pl/group-agg ["account_id"]
                  [(pl/alias (pl/sum (pl/col "notional")) "gross")
                   (pl/alias (pl/count (pl/col "trade_id")) "n")])
    (pl/sort ["gross"] true)
    (pl/collect)
    (pl/rows))
```

Expressions are plain functions building plan nodes: `pl/col`, `pl/lit`,
arithmetic folds (`pl/+ pl/- pl/* pl/div`), comparisons, `pl/if-else`,
aggregations, `pl/over` windows, `pl/cum-sum`, `pl/cast`, null and string
helpers. Literals coerce automatically in expression positions.

## As-of joins and windows: a real derivation

fibo-gen-clj's `position_eod` (daily positions from trades and prices) is
the package's exit-criterion test — the full program is
`test/cljrs/polars_fibo.cljrs` in the package repo. The essential moves:

```clojure
;; business-day spine × held (account, instrument) pairs
(def spine (pl/join bdays holdings {:how :cross}))

;; per-key running totals: sort, then a windowed cumulative sum
(-> joined
    (pl/sort ["account_id" "instrument_id" "as_of_date"])
    (pl/with-columns
     [(pl/alias (pl/over (pl/cum-sum (pl/col "qty_delta"))
                         [(pl/col "account_id") (pl/col "instrument_id")])
                "quantity")]))

;; forward-fill weekly prices onto daily rows, per instrument
(pl/join-asof positions prices
              {:on "as_of_date" :by ["instrument_id"] :strategy :backward})
```

It reproduces DuckDB's 1.2M-row output to float epsilon.

## SQL, when that reads better

```clojure
(-> (pl/sql {"nav" nav}
            "SELECT fund_id, MAX(gross_nav) AS peak
             FROM nav GROUP BY fund_id ORDER BY fund_id")
    (pl/collect)
    (pl/rows))
;; => [{:fund_id 1, :peak 3467541258.2665} ...]
```

`pl/sql` takes a map of table names to frames and returns a lazy frame;
it speaks Polars' SQL subset, not a full dialect.

## Caveats

- `shape`/`head`/`rows`/`column` need a collected frame; everything else
  is lazy.
- Polars memory is invisible to the runtime GC's limits; a
  `collect-streaming` variant runs the streaming engine for
  larger-than-memory pipelines.
- A Polars *panic* aborts the process (Rust panics don't unwind across the
  dylib boundary); ordinary errors arrive as Clojure exceptions.
