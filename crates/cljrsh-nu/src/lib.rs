//! Embedded nushell engine for cljrsh — the `nu` Clojure namespace.
//!
//! `(nu/eval "ls | where size > 1kb")` parses and evaluates a nu pipeline
//! **in-process** (no external `nu` binary) with the standard command set and
//! returns Clojure data (records → keyword-keyed maps, tables → vectors of
//! maps). State — `def`s, `let`s, env, nu-side `cd` — persists across evals
//! within a session; the implicit default session re-syncs cwd/env from the
//! process at each eval, and `(nu/session {...})` creates sticky explicit
//! sessions. `:in` supplies the pipeline's `$in`. v1 collects pipelines to a
//! value (no streaming); externals (`^ls`) run and their output lands in the
//! result.
//!
//! Evaluation is synchronous on the calling thread. `EngineState`/`Stack`
//! live behind a Mutex inside a `NuSession` NativeObject (no GC pointers →
//! trivial `Trace`). Config files are never loaded.

use std::sync::{Arc, Mutex, OnceLock, atomic::AtomicBool};

use cljrs_env::env::GlobalEnv;
use cljrs_gc::{MarkVisitor, Trace};
use cljrs_interop::{Registry, wrap_fn_variadic};
use cljrs_value::{Keyword, NativeObject, Value};
use nu_protocol::engine::{EngineState, Stack, StateWorkingSet};
use nu_protocol::{PipelineData, Signals, Span, Value as NuValue};

pub mod convert;

pub const NS: &str = "nu";

// ── Sessions ──────────────────────────────────────────────────────────────────

pub struct NuSession {
    state: Mutex<(EngineState, Stack)>,
    /// Sticky sessions keep their creation-time cwd/env; the default session
    /// re-syncs from the process on every eval.
    sticky: bool,
}

impl std::fmt::Debug for NuSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NuSession {{ sticky: {} }}", self.sticky)
    }
}

impl Trace for NuSession {
    fn trace(&self, _visitor: &mut MarkVisitor) {}
}

impl NativeObject for NuSession {
    fn type_tag(&self) -> &str {
        "NuSession"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The interrupt flag shared by every session; the hosting binary's SIGINT
/// handler can flip it to stop a running pipeline.
pub fn default_interrupt_flag() -> Arc<AtomicBool> {
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    FLAG.get_or_init(|| Arc::new(AtomicBool::new(false)))
        .clone()
}

fn build_engine(cwd: Option<&str>, env: &[(String, String)]) -> NuSession {
    let mut engine_state = nu_cmd_lang::create_default_context();
    engine_state = nu_command::add_shell_command_context(engine_state);
    engine_state.set_signals(Signals::new(default_interrupt_flag()));
    let span = Span::unknown();
    for (k, v) in std::env::vars() {
        engine_state.add_env_var(k, NuValue::string(v, span));
    }
    for (k, v) in env {
        engine_state.add_env_var(k.clone(), NuValue::string(v.clone(), span));
    }
    let pwd = cwd
        .map(str::to_string)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string())
        })
        .unwrap_or_else(|| "/".to_string());
    engine_state.add_env_var("PWD".into(), NuValue::string(pwd, span));
    NuSession {
        state: Mutex::new((engine_state, Stack::new())),
        sticky: cwd.is_some() || !env.is_empty(),
    }
}

fn default_session() -> &'static NuSession {
    static DEFAULT: OnceLock<NuSession> = OnceLock::new();
    DEFAULT.get_or_init(|| {
        let mut s = build_engine(None, &[]);
        s.sticky = false;
        s
    })
}

// ── Evaluation ────────────────────────────────────────────────────────────────

/// Parse + eval `code` in `session`, with `input` as the pipeline's `$in`.
fn eval_in_session(
    session: &NuSession,
    code: &str,
    input: PipelineData,
) -> Result<NuValue, String> {
    let (engine_state, stack) = &mut *session.state.lock().unwrap();

    if !session.sticky {
        // Default session: mirror the process cwd/env each eval.
        let span = Span::unknown();
        for (k, v) in std::env::vars() {
            engine_state.add_env_var(k, NuValue::string(v, span));
        }
        if let Ok(cwd) = std::env::current_dir() {
            engine_state.add_env_var(
                "PWD".into(),
                NuValue::string(cwd.display().to_string(), span),
            );
        }
    }

    // Parse; persist new defs/aliases into the engine state.
    let block = {
        let mut working_set = StateWorkingSet::new(engine_state);
        let block = nu_parser::parse(&mut working_set, Some("<nu-eval>"), code.as_bytes(), false);
        if let Some(err) = working_set.parse_errors.first() {
            return Err(format!("nu parse error: {err}"));
        }
        let delta = working_set.render();
        engine_state
            .merge_delta(delta)
            .map_err(|e| format!("nu state merge failed: {e}"))?;
        block
    };

    let result = nu_engine::eval_block::<nu_protocol::debugger::WithoutDebug>(
        engine_state,
        stack,
        &block,
        input,
    )
    .map_err(|e| format!("nu error: {e}"))?;

    // Fold env changes (cd, export-env) back so they persist in the session.
    engine_state
        .merge_env(stack)
        .map_err(|e| format!("nu env merge failed: {e}"))?;

    result
        .body
        .into_value(Span::unknown())
        .map_err(|e| format!("nu error: {e}"))
}

// ── Registration ──────────────────────────────────────────────────────────────

fn opt_map(v: &Value) -> Result<&cljrs_value::value::MapValue, String> {
    match v {
        Value::Map(m) => Ok(m),
        other => Err(format!("options must be a map, got {}", other.type_name())),
    }
}

fn kw(name: &str) -> Value {
    Value::keyword(Keyword::simple(name))
}

/// Register the `nu` namespace into `globals`. Idempotent.
pub fn init(globals: &Arc<GlobalEnv>) {
    if globals.is_loaded(NS) {
        return;
    }
    globals.get_or_create_ns(NS);
    globals.refer_all(NS, "clojure.core");
    let mut registry = Registry::for_require(globals.clone());
    register(&mut registry);
}

pub fn register(registry: &mut Registry) {
    // (nu/eval code) / (nu/eval code {:session s :in data :keywordize? bool})
    registry.define(
        "nu/eval",
        wrap_fn_variadic("nu/eval", 1, |args: &[Value]| -> Result<Value, String> {
            let Value::Str(code) = &args[0] else {
                return Err(format!(
                    "nu/eval expects a nu source string, got {}",
                    args[0].type_name()
                ));
            };
            let mut keywordize = true;
            let mut session_obj: Option<Value> = None;
            let mut input: Option<Value> = None;
            if let Some(opts) = args.get(1) {
                let m = opt_map(opts)?;
                if let Some(Value::Bool(false)) = m.get(&kw("keywordize?")) {
                    keywordize = false;
                }
                session_obj = m.get(&kw("session"));
                input = m.get(&kw("in"));
            }
            let code = code.get().to_string();

            let pipeline_input = match &input {
                None => PipelineData::empty(),
                Some(v) => PipelineData::value(convert::clj_to_nu(v)?, None),
            };

            let run = |session: &NuSession| -> Result<Value, String> {
                let nu_result = eval_in_session(session, &code, pipeline_input)?;
                convert::nu_to_clj(&nu_result, keywordize)
            };

            match session_obj {
                None => run(default_session()),
                Some(Value::NativeObject(obj)) => {
                    let session = obj
                        .get()
                        .downcast_ref::<NuSession>()
                        .ok_or_else(|| ":session must be a NuSession".to_string())?;
                    run(session)
                }
                Some(other) => Err(format!(
                    ":session must be a NuSession, got {}",
                    other.type_name()
                )),
            }
        }),
    );

    // (nu/session) / (nu/session {:cwd "..." :env {...}})
    registry.define(
        "nu/session",
        wrap_fn_variadic("nu/session", 0, |args: &[Value]| -> Result<Value, String> {
            let mut cwd: Option<String> = None;
            let mut env: Vec<(String, String)> = Vec::new();
            if let Some(opts) = args.first() {
                let m = opt_map(opts)?;
                if let Some(Value::Str(s)) = m.get(&kw("cwd")) {
                    cwd = Some(s.get().to_string());
                }
                if let Some(Value::Map(em)) = m.get(&kw("env")) {
                    for (k, v) in em.iter() {
                        let key = match &k {
                            Value::Str(s) => s.get().to_string(),
                            Value::Keyword(kx) => kx.get().name.to_string(),
                            other => {
                                return Err(format!(
                                    ":env keys must be strings, got {}",
                                    other.type_name()
                                ));
                            }
                        };
                        let Value::Str(vs) = &v else {
                            return Err(":env values must be strings".to_string());
                        };
                        env.push((key, vs.get().to_string()));
                    }
                }
            }
            let mut session = build_engine(cwd.as_deref(), &env);
            session.sticky = true;
            Ok(Value::NativeObject(cljrs_value::gc_native_object(session)))
        }),
    );

    // (nu/parse code) — syntax check; nil on success, throws on error.
    registry.define(
        "nu/parse",
        wrap_fn_variadic("nu/parse", 1, |args: &[Value]| -> Result<Value, String> {
            let Value::Str(code) = &args[0] else {
                return Err(format!(
                    "nu/parse expects a string, got {}",
                    args[0].type_name()
                ));
            };
            let session = default_session();
            let (engine_state, _) = &mut *session.state.lock().unwrap();
            let mut working_set = StateWorkingSet::new(engine_state);
            nu_parser::parse(
                &mut working_set,
                Some("<nu-parse>"),
                code.get().as_bytes(),
                false,
            );
            if let Some(err) = working_set.parse_errors.first() {
                return Err(format!("nu parse error: {err}"));
            }
            Ok(Value::Nil)
        }),
    );

    registry.env().mark_loaded(NS);
}
