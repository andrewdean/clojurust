//! Bulk numeric kernels over `Value::DoubleArray` / `Value::LongArray`.
//!
//! Every kernel is a plain Rust loop over a contiguous slice — one namespace
//! call per *array*, not per element, which is where numpy-class throughput
//! comes from (rustc/LLVM auto-vectorizes these loops). Kernels are pure:
//! they read inputs under the array mutex and return freshly allocated
//! arrays.

use std::sync::Mutex;

use cljrs_gc::GcPtr;
use cljrs_value::Value;

/// Either a per-element array operand or a broadcast scalar.
pub enum Operand {
    Arr(Vec<f64>),
    Scalar(f64),
}

pub fn doubles(v: &Value) -> Result<Vec<f64>, String> {
    match v {
        Value::DoubleArray(a) => Ok(a.get().lock().unwrap().clone()),
        other => Err(format!("expected double-array, got {}", other.type_name())),
    }
}

pub fn longs(v: &Value) -> Result<Vec<i64>, String> {
    match v {
        Value::LongArray(a) => Ok(a.get().lock().unwrap().clone()),
        other => Err(format!("expected long-array, got {}", other.type_name())),
    }
}

pub fn operand(v: &Value) -> Result<Operand, String> {
    match v {
        Value::DoubleArray(a) => Ok(Operand::Arr(a.get().lock().unwrap().clone())),
        Value::Double(x) => Ok(Operand::Scalar(*x)),
        Value::Long(n) => Ok(Operand::Scalar(*n as f64)),
        other => Err(format!(
            "expected double-array or number, got {}",
            other.type_name()
        )),
    }
}

pub fn da(v: Vec<f64>) -> Value {
    Value::DoubleArray(GcPtr::new(Mutex::new(v)))
}

pub fn la(v: Vec<i64>) -> Value {
    Value::LongArray(GcPtr::new(Mutex::new(v)))
}

pub fn zip_with(a: &Value, b: &Value, f: impl Fn(f64, f64) -> f64) -> Result<Value, String> {
    let xs = doubles(a)?;
    match operand(b)? {
        Operand::Scalar(s) => Ok(da(xs.iter().map(|&x| f(x, s)).collect())),
        Operand::Arr(ys) => {
            if xs.len() != ys.len() {
                return Err(format!("length mismatch: {} vs {}", xs.len(), ys.len()));
            }
            Ok(da(xs
                .iter()
                .zip(ys.iter())
                .map(|(&x, &y)| f(x, y))
                .collect()))
        }
    }
}

pub fn map_unary(a: &Value, f: impl Fn(f64) -> f64) -> Result<Value, String> {
    Ok(da(doubles(a)?.iter().map(|&x| f(x)).collect()))
}

pub fn cumsum(a: &Value) -> Result<Value, String> {
    let xs = doubles(a)?;
    let mut acc = 0.0;
    Ok(da(xs
        .iter()
        .map(|&x| {
            acc += x;
            acc
        })
        .collect()))
}

pub fn sum(a: &Value) -> Result<f64, String> {
    Ok(doubles(a)?.iter().sum())
}

/// Round half away from zero to `decimals` places (matches the reference
/// round-to used throughout fibo-gen-clj).
pub fn round(a: &Value, decimals: i64) -> Result<Value, String> {
    let scale = 10f64.powi(decimals as i32);
    map_unary(a, |x| (x * scale).round() / scale)
}

/// out[0] = seed, out[i] = a[i-1] — a one-step lag (e.g. previous close).
pub fn lag(a: &Value, seed: f64) -> Result<Value, String> {
    let xs = doubles(a)?;
    let mut out = Vec::with_capacity(xs.len());
    let mut prev = seed;
    for &x in &xs {
        out.push(prev);
        prev = x;
    }
    Ok(da(out))
}

/// Every k-th element, starting at index 0 (e.g. weekly from daily).
pub fn stride(a: &Value, k: i64) -> Result<Value, String> {
    if k <= 0 {
        return Err(format!("stride must be positive, got {k}"));
    }
    let xs = doubles(a)?;
    Ok(da(xs.iter().step_by(k as usize).copied().collect()))
}

pub fn constant(n: i64, x: f64) -> Result<Value, String> {
    if n < 0 {
        return Err(format!("length must be non-negative, got {n}"));
    }
    Ok(da(vec![x; n as usize]))
}

/// Truncate toward zero, like `(long x)` / Python `int()`.
pub fn to_longs(a: &Value) -> Result<Value, String> {
    Ok(la(doubles(a)?.iter().map(|&x| x as i64).collect()))
}

pub fn to_doubles(a: &Value) -> Result<Value, String> {
    Ok(da(longs(a)?.iter().map(|&x| x as f64).collect()))
}

/// Sequential longs [start, start+n) — for surrogate id columns.
pub fn iota(n: i64, start: i64) -> Result<Value, String> {
    if n < 0 {
        return Err(format!("length must be non-negative, got {n}"));
    }
    Ok(la((start..start + n).collect()))
}

/// Elementwise max(a[i], x) for long arrays (e.g. max(1, poisson)).
pub fn lclamp_min(a: &Value, x: i64) -> Result<Value, String> {
    Ok(la(longs(a)?.iter().map(|&v| v.max(x)).collect()))
}

/// Run-length expansion: for each i, emit i counts[i] times
/// (e.g. a per-day count vector becomes a per-pick day-index column).
pub fn expand_counts(counts: &Value) -> Result<Value, String> {
    let cs = longs(counts)?;
    let total: usize = cs.iter().map(|&c| c.max(0) as usize).sum();
    let mut out = Vec::with_capacity(total);
    for (i, &c) in cs.iter().enumerate() {
        for _ in 0..c.max(0) {
            out.push(i as i64);
        }
    }
    Ok(la(out))
}

fn checked_idx(idx: i64, len: usize, what: &str) -> Result<usize, String> {
    if idx < 0 || idx as usize >= len {
        return Err(format!("{what} index {idx} out of bounds for length {len}"));
    }
    Ok(idx as usize)
}

/// Gather doubles: out[i] = a[idxs[i]].
pub fn dtake(a: &Value, idxs: &Value) -> Result<Value, String> {
    let xs = doubles(a)?;
    let is = longs(idxs)?;
    let mut out = Vec::with_capacity(is.len());
    for &i in &is {
        out.push(xs[checked_idx(i, xs.len(), "dtake")?]);
    }
    Ok(da(out))
}

/// Gather longs: out[i] = a[idxs[i]].
pub fn ltake(a: &Value, idxs: &Value) -> Result<Value, String> {
    let xs = longs(a)?;
    let is = longs(idxs)?;
    let mut out = Vec::with_capacity(is.len());
    for &i in &is {
        out.push(xs[checked_idx(i, xs.len(), "ltake")?]);
    }
    Ok(la(out))
}

/// 2-D gather over a row-major flattened matrix:
/// out[i] = mat[rows[i] * row-len + cols[i]].
pub fn gather2d(mat: &Value, rows: &Value, cols: &Value, row_len: i64) -> Result<Value, String> {
    if row_len <= 0 {
        return Err(format!("row-len must be positive, got {row_len}"));
    }
    let m = doubles(mat)?;
    let rs = longs(rows)?;
    let cs = longs(cols)?;
    if rs.len() != cs.len() {
        return Err(format!(
            "rows/cols length mismatch: {} vs {}",
            rs.len(),
            cs.len()
        ));
    }
    let n_rows = m.len() / row_len as usize;
    let mut out = Vec::with_capacity(rs.len());
    for (&r, &c) in rs.iter().zip(cs.iter()) {
        let c = checked_idx(c, row_len as usize, "gather2d col")?;
        let r = checked_idx(r, n_rows, "gather2d row")?;
        out.push(m[r * row_len as usize + c]);
    }
    Ok(da(out))
}

/// Indices where the value is finite and strictly positive
/// (the "has a usable price" mask).
pub fn where_pos(a: &Value) -> Result<Value, String> {
    Ok(la(doubles(a)?
        .iter()
        .enumerate()
        .filter(|&(_, &x)| x.is_finite() && x > 0.0)
        .map(|(i, _)| i as i64)
        .collect()))
}

/// Elementwise select: out[i] = if a[i] < t { then } else { else_ }.
/// `then`/`else_` broadcast when scalar.
pub fn where_lt(a: &Value, t: f64, then: &Value, else_: &Value) -> Result<Value, String> {
    let xs = doubles(a)?;
    let th = operand(then)?;
    let el = operand(else_)?;
    for op in [&th, &el] {
        if let Operand::Arr(v) = op
            && v.len() != xs.len()
        {
            return Err(format!("length mismatch: {} vs {}", xs.len(), v.len()));
        }
    }
    let pick = |op: &Operand, i: usize| -> f64 {
        match op {
            Operand::Scalar(s) => *s,
            Operand::Arr(v) => v[i],
        }
    };
    Ok(da(xs
        .iter()
        .enumerate()
        .map(|(i, &x)| if x < t { pick(&th, i) } else { pick(&el, i) })
        .collect()))
}

/// Stack rows into a row-major flattened matrix. `rows` is a Clojure vector
/// whose elements are double-arrays of length row-len or nil; nil rows are
/// filled with NaN (e.g. instruments with no price series).
pub fn stack(rows: &Value, row_len: i64) -> Result<Value, String> {
    if row_len <= 0 {
        return Err(format!("row-len must be positive, got {row_len}"));
    }
    let items: Vec<Value> = match rows {
        Value::Vector(v) => v.get().iter().cloned().collect(),
        other => return Err(format!("rows must be a vector, got {}", other.type_name())),
    };
    let n = row_len as usize;
    let mut out = Vec::with_capacity(items.len() * n);
    for (i, item) in items.iter().enumerate() {
        match item {
            Value::Nil => out.extend(std::iter::repeat_n(f64::NAN, n)),
            Value::DoubleArray(a) => {
                let v = a.get().lock().unwrap();
                if v.len() != n {
                    return Err(format!("row {i} has length {}, expected {n}", v.len()));
                }
                out.extend_from_slice(&v);
            }
            other => {
                return Err(format!(
                    "row {i} must be a double-array or nil, got {}",
                    other.type_name()
                ));
            }
        }
    }
    Ok(da(out))
}

/// Inverse of `stride`: an n-element array with `out[i*k] = a[i]` and NaN
/// everywhere else (e.g. weekly observations placed on a daily grid).
pub fn expand_stride(a: &Value, k: i64, n: i64) -> Result<Value, String> {
    if k <= 0 || n < 0 {
        return Err(format!("bad expand-stride shape: k={k}, n={n}"));
    }
    let xs = doubles(a)?;
    let mut out = vec![f64::NAN; n as usize];
    for (i, &x) in xs.iter().enumerate() {
        let idx = i * k as usize;
        if idx >= out.len() {
            break;
        }
        out[idx] = x;
    }
    Ok(da(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arr(v: &[f64]) -> Value {
        da(v.to_vec())
    }

    #[test]
    fn zip_broadcast_and_pair() {
        let a = arr(&[1.0, 2.0, 3.0]);
        let out = zip_with(&a, &Value::Double(0.5), |x, y| x * y).unwrap();
        assert_eq!(doubles(&out).unwrap(), vec![0.5, 1.0, 1.5]);
        let b = arr(&[10.0, 20.0, 30.0]);
        let out = zip_with(&a, &b, |x, y| x + y).unwrap();
        assert_eq!(doubles(&out).unwrap(), vec![11.0, 22.0, 33.0]);
    }

    #[test]
    fn cumsum_lag_stride() {
        let a = arr(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(
            doubles(&cumsum(&a).unwrap()).unwrap(),
            vec![1.0, 3.0, 6.0, 10.0, 15.0]
        );
        assert_eq!(
            doubles(&lag(&a, 9.0).unwrap()).unwrap(),
            vec![9.0, 1.0, 2.0, 3.0, 4.0]
        );
        assert_eq!(
            doubles(&stride(&a, 2).unwrap()).unwrap(),
            vec![1.0, 3.0, 5.0]
        );
    }

    #[test]
    fn gather_mask_select() {
        let ids = la(vec![10, 20, 30]);
        let idxs = la(vec![2, 0]);
        assert_eq!(longs(&ltake(&ids, &idxs).unwrap()).unwrap(), vec![30, 10]);
        let xs = arr(&[1.5, 2.5, 3.5]);
        assert_eq!(
            doubles(&dtake(&xs, &idxs).unwrap()).unwrap(),
            vec![3.5, 1.5]
        );

        let masked = arr(&[1.0, f64::NAN, -2.0, 3.0]);
        assert_eq!(longs(&where_pos(&masked).unwrap()).unwrap(), vec![0, 3]);

        let sel = where_lt(
            &arr(&[0.1, 0.9]),
            0.5,
            &Value::Double(1.0),
            &Value::Double(-1.0),
        )
        .unwrap();
        assert_eq!(doubles(&sel).unwrap(), vec![1.0, -1.0]);
    }

    #[test]
    fn stack_and_gather2d() {
        let rows = Value::Vector(GcPtr::new(cljrs_value::PersistentVector::from_iter(vec![
            arr(&[1.0, 2.0]),
            Value::Nil,
            arr(&[5.0, 6.0]),
        ])));
        let mat = stack(&rows, 2).unwrap();
        let got = gather2d(&mat, &la(vec![0, 2, 1]), &la(vec![1, 0, 1]), 2).unwrap();
        let v = doubles(&got).unwrap();
        assert_eq!(v[0], 2.0);
        assert_eq!(v[1], 5.0);
        assert!(v[2].is_nan());
    }

    #[test]
    fn counts_expansion() {
        let counts = la(vec![2, 0, 3]);
        assert_eq!(
            longs(&expand_counts(&counts).unwrap()).unwrap(),
            vec![0, 0, 2, 2, 2]
        );
        assert_eq!(
            longs(&lclamp_min(&la(vec![0, 5]), 1).unwrap()).unwrap(),
            vec![1, 5]
        );
        assert_eq!(longs(&iota(3, 100).unwrap()).unwrap(), vec![100, 101, 102]);
    }

    #[test]
    fn round_half_away() {
        let a = arr(&[1.23456, -1.5, 2.675]);
        let out = round(&a, 2).unwrap();
        let got = doubles(&out).unwrap();
        assert_eq!(got[0], 1.23);
        assert_eq!(got[1], -1.5);
    }
}
