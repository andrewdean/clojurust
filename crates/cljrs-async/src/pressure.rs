//! Async-side mirror of the process memory-pressure level (isolates plan C5).
//!
//! `cljrs_gc::pressure` derives a [`PressureLevel`] from every heap's
//! published live bytes and calls listeners on transitions.  This module
//! mirrors those transitions into a `tokio::sync::watch` channel so async
//! code, on the `LocalSet` or on the `Send` worker pool, can park until
//! pressure drops instead of polling, and exposes the level to Clojure.
//!
//! ## Clojure API (`clojure.core.async`)
//!
//! - `(memory-pressure)` → `:green`, `:yellow`, or `:red`.
//! - `(memory-pressure-below level)` → a future that resolves to the level
//!   keyword once the process level is strictly below `level` (a keyword).
//!   `(await (memory-pressure-below :red))` is how a server loop stops taking
//!   from `:conns` while load is being shed; the built-in accept loops in
//!   `cljrs-net` park on the same condition internally.

use std::sync::{Arc, OnceLock};

use cljrs_env::env::GlobalEnv;
use cljrs_gc::GcPtr;
use cljrs_gc::pressure as gc_pressure;
use cljrs_value::{Arity, Keyword, NativeFn, Value, ValueError, ValueResult};
use tokio::sync::watch;

use crate::eval_async::spawn_future;

pub use cljrs_gc::pressure::PressureLevel;

/// The one watch channel every subscriber shares.  Created on first use so
/// crates that never look at pressure pay nothing.
static WATCH: OnceLock<Arc<watch::Sender<PressureLevel>>> = OnceLock::new();

fn sender() -> &'static Arc<watch::Sender<PressureLevel>> {
    WATCH.get_or_init(|| {
        let (tx, _rx) = watch::channel(PressureLevel::Green);
        let tx = Arc::new(tx);
        let publisher = tx.clone();
        gc_pressure::on_change(move |level| {
            publisher.send_replace(level);
        });
        // The listener is registered before this sync, so a transition that
        // lands between the two is delivered twice, never missed.
        tx.send_replace(gc_pressure::level());
        tx
    })
}

/// A receiver that tracks the process pressure level; `changed().await`
/// parks until the next transition.
pub fn subscribe() -> watch::Receiver<PressureLevel> {
    sender().subscribe()
}

/// The current process-wide pressure level.
pub fn level() -> PressureLevel {
    gc_pressure::level()
}

/// Park until the level is strictly below `ceiling`; returns at once when it
/// already is.  Nothing is below Green, so a Green ceiling returns
/// immediately rather than parking forever.
pub async fn wait_until_below(ceiling: PressureLevel) {
    if ceiling == PressureLevel::Green {
        return;
    }
    let mut rx = subscribe();
    loop {
        if *rx.borrow_and_update() < ceiling {
            return;
        }
        // The sender lives in a static; `changed` can only fail if it were
        // dropped, and then there is nothing left to wait for.
        if rx.changed().await.is_err() {
            return;
        }
    }
}

fn level_keyword(level: PressureLevel) -> Value {
    Value::keyword(Keyword::simple(level.as_str()))
}

fn level_arg(args: &[Value]) -> ValueResult<PressureLevel> {
    let parsed = match args.first() {
        Some(Value::Keyword(k)) if k.get().namespace.is_none() => {
            PressureLevel::parse(&k.get().name)
        }
        _ => None,
    };
    parsed.ok_or_else(|| ValueError::WrongType {
        expected: "one of :green, :yellow, :red",
        got: args
            .first()
            .map(|v| v.type_name().to_string())
            .unwrap_or_default(),
    })
}

fn builtin_memory_pressure(_args: &[Value]) -> ValueResult<Value> {
    Ok(level_keyword(level()))
}

fn builtin_memory_pressure_below(args: &[Value]) -> ValueResult<Value> {
    let ceiling = level_arg(args)?;
    Ok(spawn_future(async move {
        wait_until_below(ceiling).await;
        Ok(level_keyword(level()))
    }))
}

/// Register the memory-pressure builtins into `ns`.
pub(crate) fn register(globals: &Arc<GlobalEnv>, ns: &str) {
    let fns: Vec<(&str, Arity, fn(&[Value]) -> ValueResult<Value>)> = vec![
        ("memory-pressure", Arity::Fixed(0), builtin_memory_pressure),
        (
            "memory-pressure-below",
            Arity::Fixed(1),
            builtin_memory_pressure_below,
        ),
    ];
    for (name, arity, func) in fns {
        let nf = NativeFn::new(name, arity, func);
        globals.intern(ns, Arc::from(name), Value::NativeFunction(GcPtr::new(nf)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_arg_accepts_only_bare_level_keywords() {
        assert_eq!(
            level_arg(&[Value::keyword(Keyword::simple("red"))]).unwrap(),
            PressureLevel::Red
        );
        assert!(level_arg(&[Value::keyword(Keyword::qualified("x", "red"))]).is_err());
        assert!(level_arg(&[Value::keyword(Keyword::simple("purple"))]).is_err());
        assert!(level_arg(&[Value::Long(2)]).is_err());
        assert!(level_arg(&[]).is_err());
    }

    #[test]
    fn green_ceiling_never_parks() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        rt.block_on(wait_until_below(PressureLevel::Green));
    }
}
