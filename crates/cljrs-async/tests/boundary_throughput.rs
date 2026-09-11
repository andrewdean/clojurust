//! Boundary copy-cost measurement behind the C5 `shared-vec` go/no-go
//! (docs/user-reachable-isolates-plan.md).
//!
//! Ignored by default because it is a measurement, not an assertion.  Run:
//!
//! ```text
//! cargo test --release -p cljrs-async --test boundary_throughput -- --ignored --nocapture
//! ```
//!
//! It prints, for a vector of small maps at three sizes, the serialize and
//! deserialize cost of one crossing and the estimated bytes the boundary
//! meter would report.  Those numbers, next to the boundary size buckets in
//! `--gc-stats`, are what the plan uses to decide when a zero-copy payload
//! form would pay for itself.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cljrs_env::env::{Env, GlobalEnv};
use cljrs_reader::Parser;
use cljrs_value::Value;
use cljrs_value::clone::{deserialize, serialize};

fn env() -> Env {
    let globals: Arc<GlobalEnv> = cljrs_interp::standard_env(None, None, None);
    Env::new(globals, "user")
}

fn eval(src: &str, env: &mut Env) -> Value {
    let mut p = Parser::new(src.to_string(), "<bench>".to_string());
    let form = p
        .parse_all()
        .expect("parse")
        .into_iter()
        .next()
        .expect("form");
    cljrs_interp::eval::eval(&form, env).expect("eval")
}

/// Time `f` over `iters` runs and return the per-run average.
fn per_run(iters: u32, mut f: impl FnMut()) -> Duration {
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    start.elapsed() / iters
}

#[test]
#[ignore = "measurement, not an assertion; run with --ignored --nocapture"]
fn boundary_copy_cost_by_payload_size() {
    let _mutator = cljrs_gc::register_mutator();
    let mut env = env();
    println!();
    println!(
        "{:>8} {:>12} {:>14} {:>14} {:>10}",
        "items", "est bytes", "serialize", "deserialize", "MB/s (ser)"
    );
    for &n in &[100usize, 10_000, 100_000] {
        let src = format!(
            "(vec (for [i (range {n})] \
               {{:id i :name (str \"item-\" i) :tags [:a :b] :score (* 1.5 i)}}))"
        );
        let v = eval(&src, &mut env);
        let iters: u32 = if n >= 100_000 { 5 } else { 50 };

        let sv = serialize(&v).expect("plain data serializes");
        let bytes = sv.byte_size();
        let ser = per_run(iters, || {
            std::hint::black_box(serialize(&v).expect("serialize"));
        });
        let de = per_run(iters, || {
            let _frame = cljrs_gc::push_alloc_frame();
            std::hint::black_box(deserialize(serialize(&v).expect("serialize")));
        }) - ser;
        let mbps = bytes as f64 / ser.as_secs_f64() / (1024.0 * 1024.0);
        println!("{n:>8} {bytes:>12} {ser:>14.1?} {de:>14.1?} {mbps:>10.0}");
    }
}
