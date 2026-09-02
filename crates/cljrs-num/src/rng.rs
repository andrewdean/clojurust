//! Deterministic per-stream RNG (splitmix64 core, FNV-1a stream seeding).
//!
//! The generator is a `NativeObject` handle created by `(cljrs.num/rng seed
//! stream)`. The algorithm — including the exact draw order of every
//! distribution — matches the pure-Clojure reference implementation in
//! fibo-gen-clj's `fibo.rng`, so scalar draws and bulk fills from the same
//! stream state produce identical sequences.

use std::any::Any;
use std::sync::Mutex;

use cljrs_gc::{GcPtr, MarkVisitor, Trace};
use cljrs_value::{NativeObject, NativeObjectBox, Value};

const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;
const MIX1: u64 = 0xBF58_476D_1CE4_E5B9;
const MIX2: u64 = 0x94D0_49BB_1331_11EB;
const FNV_OFFSET: u64 = 0xCBF2_9CE4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;
const INV_2POW53: f64 = 1.0 / 9_007_199_254_740_992.0;

pub const RNG_TAG: &str = "NumRng";

#[derive(Debug)]
pub struct NumRng {
    state: Mutex<u64>,
}

impl Trace for NumRng {
    fn trace(&self, _visitor: &mut MarkVisitor) {}
}

impl NativeObject for NumRng {
    fn type_tag(&self) -> &str {
        RNG_TAG
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(MIX1);
    z = (z ^ (z >> 27)).wrapping_mul(MIX2);
    z ^ (z >> 31)
}

fn fnv1a_64(s: &str) -> u64 {
    let mut h = FNV_OFFSET;
    for ch in s.chars() {
        h = (h ^ ch as u64).wrapping_mul(FNV_PRIME);
    }
    h
}

impl NumRng {
    pub fn new(seed: i64, stream: &str) -> Self {
        NumRng {
            state: Mutex::new(seed as u64 ^ fnv1a_64(stream)),
        }
    }

    pub fn into_value(self) -> Value {
        Value::NativeObject(GcPtr::new(NativeObjectBox::new(self)))
    }

    pub fn next_u64(&self) -> u64 {
        let mut state = self.state.lock().unwrap();
        *state = state.wrapping_add(GOLDEN);
        mix64(*state)
    }

    /// Uniform double in [0, 1).
    pub fn next_double(&self) -> f64 {
        (self.next_u64() >> 11) as f64 * INV_2POW53
    }

    /// Uniform long in [lo, hi) — numpy-style half-open interval.
    pub fn integers(&self, lo: i64, hi: i64) -> i64 {
        lo + ((self.next_u64() >> 1) % (hi - lo) as u64) as i64
    }

    pub fn uniform(&self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_double()
    }

    /// Box-Muller; `1 - u` keeps the log argument in (0, 1].
    pub fn normal(&self, mu: f64, sigma: f64) -> f64 {
        let u1 = 1.0 - self.next_double();
        let u2 = self.next_double();
        mu + sigma * (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    pub fn lognormal(&self, mu: f64, sigma: f64) -> f64 {
        self.normal(mu, sigma).exp()
    }

    /// Knuth's algorithm; fine for small lambdas.
    pub fn poisson(&self, lambda: f64) -> i64 {
        let limit = (-lambda).exp();
        let mut k: i64 = 0;
        let mut p = 1.0;
        loop {
            p *= self.next_double();
            if p <= limit {
                return k;
            }
            k += 1;
        }
    }

    pub fn fill_normal(&self, n: usize, mu: f64, sigma: f64) -> Vec<f64> {
        (0..n).map(|_| self.normal(mu, sigma)).collect()
    }

    pub fn fill_uniform(&self, n: usize, lo: f64, hi: f64) -> Vec<f64> {
        (0..n).map(|_| self.uniform(lo, hi)).collect()
    }

    pub fn fill_lognormal(&self, n: usize, mu: f64, sigma: f64) -> Vec<f64> {
        (0..n).map(|_| self.lognormal(mu, sigma)).collect()
    }

    pub fn fill_integers(&self, n: usize, lo: i64, hi: i64) -> Vec<i64> {
        (0..n).map(|_| self.integers(lo, hi)).collect()
    }

    /// n distinct indices from [0, m) — partial Fisher-Yates, order
    /// randomized. Draw-for-draw identical to fibo.rng/sample-idx!.
    pub fn sample_idx(&self, n: usize, m: usize) -> Vec<i64> {
        let n = n.min(m);
        let mut pool: Vec<i64> = (0..m as i64).collect();
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let j = self.integers(i as i64, m as i64) as usize;
            pool.swap(i, j);
            out.push(pool[i]);
        }
        out
    }
}

/// Downcast a Value to the NumRng handle, or explain what went wrong.
pub fn as_rng(v: &Value) -> Result<GcPtr<NativeObjectBox>, String> {
    match v {
        Value::NativeObject(obj) if obj.get().type_tag() == RNG_TAG => Ok(obj.clone()),
        Value::NativeObject(obj) => Err(format!(
            "expected {RNG_TAG} handle, got native object {}",
            obj.get().type_tag()
        )),
        other => Err(format!("expected {RNG_TAG} handle, got {}", other.type_name())),
    }
}

pub fn with_rng<T>(v: &Value, f: impl FnOnce(&NumRng) -> T) -> Result<T, String> {
    let obj = as_rng(v)?;
    let boxed = obj.get();
    let rng = boxed
        .downcast_ref::<NumRng>()
        .ok_or_else(|| format!("{RNG_TAG} downcast failed"))?;
    Ok(f(rng))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference vector produced by the pure-Clojure implementation
    // (fibo.rng in fibo-gen-clj): seed 20260425, stream "test".
    #[test]
    fn matches_clojure_reference_doubles() {
        let r = NumRng::new(20260425, "test");
        assert_eq!(r.next_double(), 0.16349529359881254);
        assert_eq!(r.next_double(), 0.8012672768185554);
    }

    #[test]
    fn integers_in_range() {
        let r = NumRng::new(1, "x");
        for _ in 0..1000 {
            let v = r.integers(1, 13);
            assert!((1..13).contains(&v));
        }
    }

    #[test]
    fn normal_moments_plausible() {
        let r = NumRng::new(7, "norm");
        let xs = r.fill_normal(100_000, 0.0, 1.0);
        let mean = xs.iter().sum::<f64>() / xs.len() as f64;
        let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / xs.len() as f64;
        assert!(mean.abs() < 0.02, "mean {mean}");
        assert!((var - 1.0).abs() < 0.03, "var {var}");
    }

    #[test]
    fn poisson_mean_plausible() {
        let r = NumRng::new(7, "pois");
        let n = 20_000;
        let total: i64 = (0..n).map(|_| r.poisson(6.0)).sum();
        let mean = total as f64 / n as f64;
        assert!((mean - 6.0).abs() < 0.1, "mean {mean}");
    }
}
