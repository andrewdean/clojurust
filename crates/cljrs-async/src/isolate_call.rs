//! Clojure-level isolate surface — Phase C2 of
//! `docs/user-reachable-isolates-plan.md`.
//!
//! - `(isolate)` → a handle to a fresh isolate: its own OS thread, GC heap,
//!   and `current_thread` executor. The handle is a `Resource`, therefore
//!   itself non-shareable — handles cannot leak across a boundary.
//! - `(isolate-call iso 'ns/fn & args)` → a `Future` for `(ns/fn & args)`
//!   evaluated *inside* the isolate. The symbol must be fully qualified: work
//!   ships as a symbol plus deep-copied arguments, never a closure (D1). Args
//!   and result cross the metered structured-clone boundary; a value that
//!   cannot cross raises a located error at this call site.
//! - `(isolate-close! iso)` → close the handle. Queued calls still run; new
//!   calls error. The worker thread exits after draining.
//! - `(default-isolate)` → round-robin handle from a lazily-created pool
//!   (size `CLJRS_ISOLATE_POOL_SIZE`, default `available_parallelism`),
//!   backing the `pfuture` macro.

use std::any::Any;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use cljrs_env::env::{Env, GlobalEnv};
use cljrs_env::error::EvalError;
use cljrs_gc::GcPtr;
use cljrs_value::clone::{CloneError, SerializedValue, deserialize, serialize};
use cljrs_value::resource::{Resource, ResourceHandle};
use cljrs_value::{Arity, NativeFn, Value, ValueError, ValueResult};

use crate::eval_async::{await_value, spawn_future};
use crate::isolate::Isolate;

/// Result of one `isolate-call`, sent back over the per-call oneshot.
enum IsolateReply {
    Ok(SerializedValue),
    Err(String),
}

/// One unit of work: a fully qualified symbol plus serialized arguments.
struct IsolateCmd {
    ns: String,
    name: String,
    args: Vec<SerializedValue>,
    reply: tokio::sync::oneshot::Sender<IsolateReply>,
}

/// The Clojure-visible isolate handle. Lives inside a `Value::Resource`;
/// holds only `Send + Sync` data (no `GcPtr`), so the worker's lifetime is
/// governed by the Arc, not the GC.
pub struct IsolateHandle {
    name: String,
    tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<IsolateCmd>>>,
    closed: AtomicBool,
}

impl std::fmt::Debug for IsolateHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IsolateHandle")
            .field("name", &self.name)
            .field("closed", &self.closed.load(Ordering::Relaxed))
            .finish()
    }
}

impl Resource for IsolateHandle {
    fn resource_type(&self) -> &'static str {
        "isolate"
    }
    /// Dropping the sender lets the worker drain queued calls and exit.
    fn close(&self) -> ValueResult<()> {
        self.closed.store(true, Ordering::Relaxed);
        self.tx.lock().unwrap().take();
        Ok(())
    }
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Serialize with the same boundary metering `IsolateSender::send` performs,
/// so `isolate-call` traffic shows up in `GC_STATS` like every other crossing.
fn serialize_metered(v: &Value) -> Result<SerializedValue, CloneError> {
    let start = std::time::Instant::now();
    let sv = serialize(v)?;
    cljrs_gc::GC_STATS.record_boundary_crossing(sv.byte_size() as u64, start.elapsed());
    Ok(sv)
}

static ISOLATE_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Spawn a worker isolate: OS thread + heap + executor + its own interpreter
/// environment (built from the *creator's* source paths, so `require` inside
/// the isolate sees the same code).
fn spawn_isolate(name: String, source_paths: Vec<PathBuf>) -> IsolateHandle {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<IsolateCmd>();
    Isolate::new(name.clone()).spawn(move || async move {
        let globals = cljrs_interp::standard_env_with_paths(None, None, None, source_paths);
        crate::init(&globals);
        while let Some(cmd) = rx.recv().await {
            let reply = run_cmd(&globals, cmd.ns, cmd.name, cmd.args).await;
            let _ = cmd.reply.send(reply);
        }
    });
    IsolateHandle {
        name,
        tx: Mutex::new(Some(tx)),
        closed: AtomicBool::new(false),
    }
}

/// Require the symbol's namespace, resolve it, apply it to the deserialized
/// arguments, and serialize the result back. Every failure becomes an
/// `IsolateReply::Err` carrying the phase it failed in.
async fn run_cmd(
    globals: &Arc<GlobalEnv>,
    ns: String,
    name: String,
    args: Vec<SerializedValue>,
) -> IsolateReply {
    let mut env = Env::new(globals.clone(), "user");
    let callee = {
        let src = format!("(do (require (quote {ns})) {ns}/{name})");
        let mut parser = cljrs_reader::Parser::new(src, "<isolate-call>".to_string());
        let form = match parser.parse_all() {
            Ok(forms) if !forms.is_empty() => forms.into_iter().next().unwrap(),
            _ => return IsolateReply::Err(format!("cannot parse symbol {ns}/{name}")),
        };
        match cljrs_interp::eval::eval(&form, &mut env) {
            Ok(v) => v,
            Err(e) => return IsolateReply::Err(format!("resolving {ns}/{name}: {e}")),
        }
    };
    let args: Vec<Value> = args.into_iter().map(deserialize).collect();
    // Intern the callee and args into worker-private vars and apply through
    // the async evaluator: one path that handles native fns, interpreted fns,
    // and ^:async fns uniformly (commands run serially, so fixed names are
    // safe — each worker owns its globals).
    globals.intern("user", Arc::from("isolate-call-f*"), callee);
    globals.intern(
        "user",
        Arc::from("isolate-call-args*"),
        Value::Vector(GcPtr::new(cljrs_value::PersistentVector::from_iter(args))),
    );
    let call_form = {
        let mut parser = cljrs_reader::Parser::new(
            "(apply isolate-call-f* isolate-call-args*)".to_string(),
            "<isolate-call>".to_string(),
        );
        parser.parse_all().expect("static form parses").remove(0)
    };
    let result = match crate::eval_async::eval_async(&call_form, &mut env).await {
        // An ^:async callee hands back a future; deliver its settled value.
        Ok(v @ (Value::Future(_) | Value::Promise(_))) => await_value(v).await,
        other => other,
    };
    match result {
        Ok(v) => match serialize_metered(&v) {
            Ok(sv) => IsolateReply::Ok(sv),
            Err(e) => IsolateReply::Err(format!(
                "result of {ns}/{name} cannot cross the isolate boundary: {e}"
            )),
        },
        Err(e) => IsolateReply::Err(format!("{ns}/{name}: {e}")),
    }
}

/// Source paths from the calling eval context, conveyed to new isolates.
fn caller_source_paths() -> ValueResult<Vec<PathBuf>> {
    let (globals, _ns) = cljrs_env::callback::capture_eval_context()
        .ok_or_else(|| ValueError::Other("isolate created outside an eval context".into()))?;
    Ok(globals.source_paths.read().unwrap().clone())
}

/// `(isolate)` — spawn a fresh isolate and return its handle.
fn builtin_isolate(_args: &[Value]) -> ValueResult<Value> {
    let n = ISOLATE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let handle = spawn_isolate(format!("isolate-{n}"), caller_source_paths()?);
    Ok(Value::Resource(ResourceHandle(Arc::new(handle))))
}

/// `(isolate? v)` — true for an open-or-closed isolate handle.
fn builtin_isolate_p(args: &[Value]) -> ValueResult<Value> {
    Ok(Value::Bool(matches!(
        args.first(),
        Some(Value::Resource(rh)) if rh.0.as_any().is::<IsolateHandle>()
    )))
}

fn handle_arg<'a>(args: &'a [Value], who: &str) -> ValueResult<&'a IsolateHandle> {
    match args.first() {
        Some(Value::Resource(rh)) => rh.0.as_any().downcast_ref::<IsolateHandle>().ok_or_else(|| {
            ValueError::Other(format!("{who}: expected an isolate handle resource"))
        }),
        other => Err(ValueError::WrongType {
            expected: "isolate handle",
            got: other.map(|v| v.type_name().to_string()).unwrap_or_default(),
        }),
    }
}

/// `(isolate-call iso 'ns/fn & args)` — run `(ns/fn & args)` inside `iso`,
/// returning a `Future`. The distinct parallel primitive: `future` stays
/// loop-async and is never re-interpreted across the boundary.
fn builtin_isolate_call(args: &[Value]) -> ValueResult<Value> {
    let handle = handle_arg(args, "isolate-call")?;
    let sym = match args.get(1) {
        Some(Value::Symbol(s)) => s.get().clone(),
        other => {
            return Err(ValueError::WrongType {
                expected: "fully qualified symbol",
                got: other.map(|v| v.type_name().to_string()).unwrap_or_default(),
            });
        }
    };
    let Some(ns) = sym.namespace.clone() else {
        return Err(ValueError::Other(format!(
            "isolate-call: symbol {} must be fully qualified (ns/fn) — work \
             ships to an isolate by name, never by closure",
            sym.name
        )));
    };
    let mut sargs = Vec::with_capacity(args.len().saturating_sub(2));
    for (i, a) in args.iter().skip(2).enumerate() {
        match serialize_metered(a) {
            Ok(sv) => sargs.push(sv),
            Err(e) => {
                return Err(ValueError::Other(format!(
                    "isolate-call: argument {i} {e}; the value holds \
                     isolate-local state and cannot cross an isolate boundary"
                )));
            }
        }
    }
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    let cmd = IsolateCmd {
        ns: ns.to_string(),
        name: sym.name.to_string(),
        args: sargs,
        reply: reply_tx,
    };
    let iso_name = handle.name.clone();
    let sent = handle
        .tx
        .lock()
        .unwrap()
        .as_ref()
        .map(|tx| tx.send(cmd).is_ok())
        .unwrap_or(false);
    if !sent {
        return Err(ValueError::Other(format!(
            "isolate-call: isolate {iso_name} is closed"
        )));
    }
    Ok(spawn_future(async move {
        match reply_rx.await {
            Ok(IsolateReply::Ok(sv)) => Ok(deserialize(sv)),
            Ok(IsolateReply::Err(msg)) => {
                Err(EvalError::Runtime(format!("isolate {iso_name}: {msg}")))
            }
            // Sender dropped without a reply: the worker died mid-call.
            Err(_) => Err(EvalError::Runtime(format!(
                "isolate {iso_name} died before replying"
            ))),
        }
    }))
}

/// `(isolate-close! iso)` — close the handle; queued calls drain, the worker
/// thread exits, new calls error.
fn builtin_isolate_close(args: &[Value]) -> ValueResult<Value> {
    handle_arg(args, "isolate-close!")?.close()?;
    Ok(Value::Nil)
}

// ── Default pool (backs `pfuture`) ───────────────────────────────────────────

thread_local! {
    /// Per-isolate pool of worker handles; `Value::Resource` holds no `GcPtr`,
    /// so thread-local storage is safe (the Arc, not the GC, owns the worker).
    static DEFAULT_POOL: RefCell<Vec<Value>> = const { RefCell::new(Vec::new()) };
    static POOL_NEXT: Cell<usize> = const { Cell::new(0) };
}

fn pool_size() -> usize {
    std::env::var("CLJRS_ISOLATE_POOL_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
        })
}

/// `(default-isolate)` — next pool handle, round-robin. The pool spawns
/// lazily on first use.
fn builtin_default_isolate(_args: &[Value]) -> ValueResult<Value> {
    let paths = caller_source_paths()?;
    DEFAULT_POOL.with(|p| {
        let mut pool = p.borrow_mut();
        if pool.is_empty() {
            for i in 0..pool_size() {
                let handle = spawn_isolate(format!("pool-{i}"), paths.clone());
                pool.push(Value::Resource(ResourceHandle(Arc::new(handle))));
            }
        }
        let i = POOL_NEXT.with(|c| {
            let i = c.get();
            c.set(i.wrapping_add(1));
            i
        });
        Ok(pool[i % pool.len()].clone())
    })
}

/// Register the isolate-call builtins into `ns`.
pub(crate) fn register(globals: &Arc<GlobalEnv>, ns: &str) {
    let fns: Vec<(&str, Arity, fn(&[Value]) -> ValueResult<Value>)> = vec![
        ("isolate", Arity::Fixed(0), builtin_isolate),
        ("isolate?", Arity::Fixed(1), builtin_isolate_p),
        ("isolate-call", Arity::Variadic { min: 2 }, builtin_isolate_call),
        ("isolate-close!", Arity::Fixed(1), builtin_isolate_close),
        ("default-isolate", Arity::Fixed(0), builtin_default_isolate),
    ];
    for (name, arity, func) in fns {
        let nf = NativeFn::new(name, arity, func);
        globals.intern(ns, Arc::from(name), Value::NativeFunction(GcPtr::new(nf)));
    }
}
