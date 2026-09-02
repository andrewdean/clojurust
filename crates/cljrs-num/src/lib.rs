//! `cljrs.num` — bulk numeric kernels, deterministic RNG, and bulk CSV
//! emission for clojurust.
//!
//! The design premise is numpy's: array workloads get fast by dispatching
//! once per *array* into native loops over contiguous unboxed buffers, not
//! by making per-element interpretation faster. Arrays are the runtime's
//! existing `double-array` / `long-array` values, so kernels compose with
//! `aget`/`aset`/`amap` and ordinary Clojure code.
//!
//! See README.md for the full function catalog and examples.

mod csv;
mod kernels;
mod rng;

use std::sync::Arc;

use cljrs_env::env::GlobalEnv;
use cljrs_interop::Registry;
use cljrs_value::{Arity, NativeFn, Value, ValueError};

use kernels as k;
use rng::{NumRng, with_rng};

pub const NS: &str = "cljrs.num";

/// Register the `cljrs.num` namespace into `globals`.
///
/// Idempotent: the namespace is built only on the first call.
pub fn init(globals: &Arc<GlobalEnv>) {
    if globals.is_loaded(NS) {
        return;
    }
    globals.get_or_create_ns(NS);
    globals.refer_all(NS, "clojure.core");
    let registry = Registry::for_require(globals.clone());
    register(&registry);
}

fn err(e: impl std::fmt::Display) -> ValueError {
    ValueError::Other(e.to_string())
}

/// Fixed-arity NativeFn from a `&[Value]` closure (the interop wrap_fnN
/// helpers stop at three arguments).
fn fixed(
    name: &'static str,
    n: usize,
    f: impl Fn(&[Value]) -> Result<Value, String> + Send + Sync + 'static,
) -> NativeFn {
    NativeFn::with_closure(name, Arity::Fixed(n), move |args| f(args).map_err(err))
}

fn f64_arg(args: &[Value], i: usize) -> Result<f64, String> {
    match &args[i] {
        Value::Double(x) => Ok(*x),
        Value::Long(n) => Ok(*n as f64),
        other => Err(format!(
            "argument {i}: expected number, got {}",
            other.type_name()
        )),
    }
}

fn i64_arg(args: &[Value], i: usize) -> Result<i64, String> {
    match &args[i] {
        Value::Long(n) => Ok(*n),
        other => Err(format!(
            "argument {i}: expected integer, got {}",
            other.type_name()
        )),
    }
}

fn str_arg(args: &[Value], i: usize) -> Result<String, String> {
    match &args[i] {
        Value::Str(s) => Ok(s.get().clone()),
        other => Err(format!(
            "argument {i}: expected string, got {}",
            other.type_name()
        )),
    }
}

fn usize_arg(args: &[Value], i: usize) -> Result<usize, String> {
    let n = i64_arg(args, i)?;
    if n < 0 {
        return Err(format!("argument {i}: expected non-negative size, got {n}"));
    }
    Ok(n as usize)
}

fn def_zip(registry: &Registry, name: &'static str, f: fn(f64, f64) -> f64) {
    registry.define_in(
        NS,
        name,
        fixed(name, 2, move |args| k::zip_with(&args[0], &args[1], f)),
    );
}

fn def_unary(registry: &Registry, name: &'static str, f: fn(f64) -> f64) {
    registry.define_in(
        NS,
        name,
        fixed(name, 1, move |args| k::map_unary(&args[0], f)),
    );
}

pub fn register(registry: &Registry) {
    // ── RNG ─────────────────────────────────────────────────────────────
    registry.define_in(
        NS,
        "rng",
        fixed("rng", 2, |args| {
            let seed = i64_arg(args, 0)?;
            let stream = str_arg(args, 1)?;
            Ok(NumRng::new(seed, &stream).into_value())
        }),
    );
    registry.define_in(
        NS,
        "next-double!",
        fixed("next-double!", 1, |args| {
            with_rng(&args[0], |r| Value::Double(r.next_double()))
        }),
    );
    registry.define_in(
        NS,
        "next-long!",
        fixed("next-long!", 1, |args| {
            with_rng(&args[0], |r| Value::Long(r.next_u64() as i64))
        }),
    );
    registry.define_in(
        NS,
        "integers!",
        fixed("integers!", 3, |args| {
            let (lo, hi) = (i64_arg(args, 1)?, i64_arg(args, 2)?);
            if hi <= lo {
                return Err(format!("empty range [{lo}, {hi})"));
            }
            with_rng(&args[0], |r| Value::Long(r.integers(lo, hi)))
        }),
    );
    registry.define_in(
        NS,
        "uniform!",
        fixed("uniform!", 3, |args| {
            let (lo, hi) = (f64_arg(args, 1)?, f64_arg(args, 2)?);
            with_rng(&args[0], |r| Value::Double(r.uniform(lo, hi)))
        }),
    );
    registry.define_in(
        NS,
        "normal!",
        fixed("normal!", 3, |args| {
            let (mu, sigma) = (f64_arg(args, 1)?, f64_arg(args, 2)?);
            with_rng(&args[0], |r| Value::Double(r.normal(mu, sigma)))
        }),
    );
    registry.define_in(
        NS,
        "lognormal!",
        fixed("lognormal!", 3, |args| {
            let (mu, sigma) = (f64_arg(args, 1)?, f64_arg(args, 2)?);
            with_rng(&args[0], |r| Value::Double(r.lognormal(mu, sigma)))
        }),
    );
    registry.define_in(
        NS,
        "poisson!",
        fixed("poisson!", 2, |args| {
            let lambda = f64_arg(args, 1)?;
            with_rng(&args[0], |r| Value::Long(r.poisson(lambda)))
        }),
    );
    registry.define_in(
        NS,
        "fill-normal!",
        fixed("fill-normal!", 4, |args| {
            let n = usize_arg(args, 1)?;
            let (mu, sigma) = (f64_arg(args, 2)?, f64_arg(args, 3)?);
            with_rng(&args[0], |r| k::da(r.fill_normal(n, mu, sigma)))
        }),
    );
    registry.define_in(
        NS,
        "fill-uniform!",
        fixed("fill-uniform!", 4, |args| {
            let n = usize_arg(args, 1)?;
            let (lo, hi) = (f64_arg(args, 2)?, f64_arg(args, 3)?);
            with_rng(&args[0], |r| k::da(r.fill_uniform(n, lo, hi)))
        }),
    );
    registry.define_in(
        NS,
        "fill-lognormal!",
        fixed("fill-lognormal!", 4, |args| {
            let n = usize_arg(args, 1)?;
            let (mu, sigma) = (f64_arg(args, 2)?, f64_arg(args, 3)?);
            with_rng(&args[0], |r| k::da(r.fill_lognormal(n, mu, sigma)))
        }),
    );
    registry.define_in(
        NS,
        "sample-idx!",
        fixed("sample-idx!", 3, |args| {
            let n = usize_arg(args, 1)?;
            let m = usize_arg(args, 2)?;
            with_rng(&args[0], |r| k::la(r.sample_idx(n, m)))
        }),
    );
    registry.define_in(
        NS,
        "fill-poisson!",
        fixed("fill-poisson!", 3, |args| {
            let n = usize_arg(args, 1)?;
            let lambda = f64_arg(args, 2)?;
            with_rng(&args[0], |r| k::la(r.fill_poisson(n, lambda)))
        }),
    );
    registry.define_in(
        NS,
        "sample-groups!",
        fixed("sample-groups!", 3, |args| {
            let counts = k::longs(&args[1])?;
            let m = usize_arg(args, 2)?;
            with_rng(&args[0], |r| k::la(r.sample_groups(&counts, m)))
        }),
    );
    registry.define_in(
        NS,
        "fill-integers!",
        fixed("fill-integers!", 4, |args| {
            let n = usize_arg(args, 1)?;
            let (lo, hi) = (i64_arg(args, 2)?, i64_arg(args, 3)?);
            if hi <= lo {
                return Err(format!("empty range [{lo}, {hi})"));
            }
            with_rng(&args[0], |r| k::la(r.fill_integers(n, lo, hi)))
        }),
    );

    // ── elementwise ─────────────────────────────────────────────────────
    def_zip(registry, "add", |x, y| x + y);
    def_zip(registry, "sub", |x, y| x - y);
    def_zip(registry, "mul", |x, y| x * y);
    def_zip(registry, "div", |x, y| x / y);
    def_zip(registry, "emax", f64::max);
    def_zip(registry, "emin", f64::min);
    def_zip(registry, "clamp-min", f64::max);
    def_zip(registry, "clamp-max", f64::min);
    def_unary(registry, "exp", f64::exp);
    def_unary(registry, "log", f64::ln);
    def_unary(registry, "log1p", f64::ln_1p);
    def_unary(registry, "sqrt", f64::sqrt);
    def_unary(registry, "abs", f64::abs);
    def_unary(registry, "neg", |x| -x);

    registry.define_in(
        NS,
        "round",
        fixed("round", 2, |args| k::round(&args[0], i64_arg(args, 1)?)),
    );

    // ── scans, reductions, shape ────────────────────────────────────────
    registry.define_in(NS, "cumsum", fixed("cumsum", 1, |args| k::cumsum(&args[0])));
    registry.define_in(
        NS,
        "sum",
        fixed("sum", 1, |args| k::sum(&args[0]).map(Value::Double)),
    );
    registry.define_in(
        NS,
        "lag",
        fixed("lag", 2, |args| k::lag(&args[0], f64_arg(args, 1)?)),
    );
    registry.define_in(
        NS,
        "stride",
        fixed("stride", 2, |args| k::stride(&args[0], i64_arg(args, 1)?)),
    );
    registry.define_in(
        NS,
        "constant",
        fixed("constant", 2, |args| {
            k::constant(i64_arg(args, 0)?, f64_arg(args, 1)?)
        }),
    );
    registry.define_in(
        NS,
        "iota",
        fixed("iota", 2, |args| {
            k::iota(i64_arg(args, 0)?, i64_arg(args, 1)?)
        }),
    );
    registry.define_in(
        NS,
        "lclamp-min",
        fixed("lclamp-min", 2, |args| {
            k::lclamp_min(&args[0], i64_arg(args, 1)?)
        }),
    );
    registry.define_in(
        NS,
        "expand-counts",
        fixed("expand-counts", 1, |args| k::expand_counts(&args[0])),
    );
    registry.define_in(
        NS,
        "dtake",
        fixed("dtake", 2, |args| k::dtake(&args[0], &args[1])),
    );
    registry.define_in(
        NS,
        "ltake",
        fixed("ltake", 2, |args| k::ltake(&args[0], &args[1])),
    );
    registry.define_in(
        NS,
        "gather2d",
        fixed("gather2d", 4, |args| {
            k::gather2d(&args[0], &args[1], &args[2], i64_arg(args, 3)?)
        }),
    );
    registry.define_in(
        NS,
        "where-pos",
        fixed("where-pos", 1, |args| k::where_pos(&args[0])),
    );
    registry.define_in(
        NS,
        "where-lt",
        fixed("where-lt", 4, |args| {
            k::where_lt(&args[0], f64_arg(args, 1)?, &args[2], &args[3])
        }),
    );
    registry.define_in(
        NS,
        "stack",
        fixed("stack", 2, |args| k::stack(&args[0], i64_arg(args, 1)?)),
    );
    registry.define_in(
        NS,
        "expand-stride",
        fixed("expand-stride", 3, |args| {
            k::expand_stride(&args[0], i64_arg(args, 1)?, i64_arg(args, 2)?)
        }),
    );
    registry.define_in(
        NS,
        "to-longs",
        fixed("to-longs", 1, |args| k::to_longs(&args[0])),
    );
    registry.define_in(
        NS,
        "to-doubles",
        fixed("to-doubles", 1, |args| k::to_doubles(&args[0])),
    );

    // ── bulk CSV ────────────────────────────────────────────────────────
    registry.define(
        "cljrs.num/write-csv!",
        NativeFn::with_closure("write-csv!", Arity::Variadic { min: 3 }, move |args| {
            let go = || -> Result<Value, String> {
                let path = str_arg(args, 0)?;
                let header: Vec<String> = match &args[1] {
                    Value::Vector(v) => v
                        .get()
                        .iter()
                        .map(|h| match h {
                            Value::Str(s) => Ok(s.get().clone()),
                            other => Err(format!(
                                "header names must be strings, got {}",
                                other.type_name()
                            )),
                        })
                        .collect::<Result<_, _>>()?,
                    other => {
                        return Err(format!(
                            "header must be a vector, got {}",
                            other.type_name()
                        ));
                    }
                };
                let specs: Vec<Value> = match &args[2] {
                    Value::Vector(v) => v.get().iter().cloned().collect(),
                    other => {
                        return Err(format!(
                            "columns must be a vector, got {}",
                            other.type_name()
                        ));
                    }
                };
                let append = matches!(args.get(3), Some(Value::Bool(true)));
                csv::write_csv(&path, &header, &specs, append).map(Value::Long)
            };
            go().map_err(err)
        }),
    );

    registry.env().mark_loaded(NS);
}
