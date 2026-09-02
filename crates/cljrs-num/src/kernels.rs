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
    fn round_half_away() {
        let a = arr(&[1.23456, -1.5, 2.675]);
        let out = round(&a, 2).unwrap();
        let got = doubles(&out).unwrap();
        assert_eq!(got[0], 1.23);
        assert_eq!(got[1], -1.5);
    }
}
