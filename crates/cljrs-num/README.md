# cljrs-num

## Purpose

Bulk numeric kernels, a deterministic per-stream RNG, and bulk CSV emission,
exposed as the `cljrs.num` namespace. One namespace call dispatches into a
native loop over a contiguous unboxed buffer (`double-array` / `long-array`),
which is where numpy-class throughput comes from — the interpreter pays one
dispatch per *array*, not per element, and rustc/LLVM auto-vectorizes the
loops.

## Status

Implemented and registered behind the default-on `num` feature of the `cljrs`
CLI crate. Motivated by the fibo-gen-clj data generator, whose per-element
interpreted hot loops ran at ~500–2500 rows/s; the same math through these
kernels runs at native speed.

## File layout

| File | Description |
|---|---|
| `src/lib.rs` | `init`/`register`: builds the `cljrs.num` namespace, arg marshalling |
| `src/rng.rs` | `NumRng` native object: splitmix64 core, FNV-1a stream seeding, scalar draws and bulk fills |
| `src/kernels.rs` | Elementwise/zip/scan kernels over `Value::DoubleArray`/`LongArray` |
| `src/csv.rs` | Column-oriented CSV writer (`write-csv!`), including epoch-day date rendering |

## Public API (Clojure)

All functions live in `cljrs.num`. Arrays are the runtime's ordinary
`double-array`/`long-array` values, so results compose with `aget`, `aset`,
`amap`, and `seq`.

### RNG

The generator matches the pure-Clojure reference implementation in
fibo-gen-clj (`fibo.rng`) draw-for-draw: splitmix64 advanced by the golden
gamma, streams seeded as `seed XOR fnv1a-64(stream)`, doubles from the top 53
bits, Box-Muller normals, Knuth poisson.

- `(rng seed stream)` → opaque handle (seed: long, stream: string)
- Scalar draws: `(next-double! r)`, `(next-long! r)`, `(integers! r lo hi)`
  (half-open), `(uniform! r lo hi)`, `(normal! r mu sigma)`,
  `(lognormal! r mu sigma)`, `(poisson! r lambda)`
- Bulk fills → new arrays: `(fill-normal! r n mu sigma)`,
  `(fill-uniform! r n lo hi)`, `(fill-lognormal! r n mu sigma)`,
  `(fill-integers! r n lo hi)`

### Elementwise (pure; second operand broadcasts when scalar)

- `(add a b)`, `(sub a b)`, `(mul a b)`, `(div a b)`, `(emax a b)`, `(emin a b)`
- `(clamp-min a x)`, `(clamp-max a x)` — aliases of emax/emin, named for intent
- `(exp a)`, `(log a)`, `(log1p a)`, `(sqrt a)`, `(abs a)`, `(neg a)`
- `(round a decimals)` — half away from zero

### Scans, reductions, shape

- `(cumsum a)`, `(sum a)`
- `(lag a seed)` — `out[0]=seed, out[i]=a[i-1]`
- `(stride a k)` — every k-th element from index 0
- `(constant n x)` — n-element array of x
- `(to-longs a)` (truncates toward zero), `(to-doubles a)`

### Bulk CSV

`(write-csv! path header cols)` / `(write-csv! path header cols append?)`
→ rows written (long). `header` is a vector of strings; `cols` is a vector of
column specs:

| Spec | Meaning |
|---|---|
| `[:d double-array]` | doubles, shortest round-trip repr; NaN → empty field |
| `[:l long-array]` | longs |
| `[:date long-array]` | epoch-day numbers rendered `YYYY-MM-DD` |
| `[:s vector]` | per-row strings/numbers/booleans (nil → empty), CSV-escaped |
| `[:const s]` | the same string for every row (nil → empty) |

With `append?` true the file is appended and the header is written only if
the file did not already exist.

### Example

```clojure
(require '[cljrs.num :as num])

;; GBM close-price path, numpy-style:
(let [r      (num/rng 20260425 "equity_prices")
      rets   (num/fill-normal! r 1305 0.0002 0.02)
      closes (num/round (num/mul (num/exp (num/cumsum rets)) 100.0) 10)]
  (num/write-csv! "closes.csv" ["day" "close"]
                  [[:date (long-array (range 18262 19567))]
                   [:d closes]]))
```

## Rust API

`cljrs_num::init(&globals)` — idempotent namespace registration (called by
the CLI when the `num` feature is enabled). `NumRng` (in `rng.rs`) is public
for reuse from other native crates.
