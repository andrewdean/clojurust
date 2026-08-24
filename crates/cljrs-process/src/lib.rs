//! Subprocess support for clojurust scripting (the `cljrs.process` namespace).
//!
//! Trusted, full-power host surface — never registered in the restricted
//! transaction environment (cljrs-tx boots only cljrs-builtins natives).
//!
//! - `(sh "cmd" "arg" ... & {:keys [in dir env]})` — run to completion,
//!   capture output: `{:exit N :out String :err String}` (clojure.java.shell
//!   semantics: trailing keyword options after the string args).
//! - `(spawn ["cmd" "arg" ...] opts?)` — start a child and return a
//!   `ChildProcess` handle. Opts: `:dir`, `:env` (replace), `:extra-env`
//!   (merge), `:in` (string piped to stdin, or `:inherit`), `:out`/`:err`
//!   (`:pipe` default, or `:inherit`).
//! - `(wait proc)` — block until exit; returns `{:exit N :out ... :err ...}`
//!   (out/err are captured strings for piped streams, nil for inherited).
//! - `(alive? proc)`, `(exit-code proc)` (nil while running),
//!   `(destroy proc)` — kill the child (the child only, not its descendants).

use std::collections::HashMap;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use cljrs_env::env::GlobalEnv;
use cljrs_gc::{GcPtr, MarkVisitor, Trace};
use cljrs_interop::{Registry, wrap_fn_variadic, wrap_fn1};
use cljrs_value::value::MapValue;
use cljrs_value::{Keyword, NativeObject, Value, gc_native_object};

pub const NS: &str = "cljrs.process";

// ── ChildProcess handle ───────────────────────────────────────────────────────

/// `(exit, out, err)` from a completed child; out/err are `None` for
/// inherited streams.
type WaitResult = (i32, Option<String>, Option<String>);

#[derive(Debug)]
pub struct ChildProcess {
    cmd: String,
    // None after wait() has reaped the child.
    child: Mutex<Option<Child>>,
    result: Mutex<Option<WaitResult>>,
}

impl Trace for ChildProcess {
    fn trace(&self, _visitor: &mut MarkVisitor) {}
}

impl NativeObject for ChildProcess {
    fn type_tag(&self) -> &str {
        "ChildProcess"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ── Value helpers ─────────────────────────────────────────────────────────────

fn kw(name: &str) -> Value {
    Value::keyword(Keyword::simple(name))
}

fn result_map(exit: i32, out: Option<String>, err: Option<String>) -> Value {
    let mut m = MapValue::empty();
    m = m.assoc(kw("exit"), Value::Long(exit as i64));
    m = m.assoc(kw("out"), out.map(Value::string).unwrap_or(Value::Nil));
    m = m.assoc(kw("err"), err.map(Value::string).unwrap_or(Value::Nil));
    Value::Map(m)
}

fn as_str(v: &Value, what: &str) -> Result<String, String> {
    match v {
        Value::Str(s) => Ok(s.get().to_string()),
        other => Err(format!(
            "{what} must be a string, got {}",
            other.type_name()
        )),
    }
}

fn keyword_name(v: &Value) -> Option<String> {
    match v {
        Value::Keyword(k) => Some(k.get().name.to_string()),
        _ => None,
    }
}

fn env_pairs(v: &Value, what: &str) -> Result<Vec<(String, String)>, String> {
    match v {
        Value::Map(m) => {
            let mut pairs = Vec::new();
            for (k, val) in m.iter() {
                let key = match &k {
                    Value::Str(s) => s.get().to_string(),
                    Value::Keyword(kx) => kx.get().name.to_string(),
                    other => {
                        return Err(format!(
                            "{what} keys must be strings or keywords, got {}",
                            other.type_name()
                        ));
                    }
                };
                pairs.push((key, as_str(val, what)?));
            }
            Ok(pairs)
        }
        other => Err(format!("{what} must be a map, got {}", other.type_name())),
    }
}

// ── Option parsing ────────────────────────────────────────────────────────────

#[derive(Default)]
struct SpawnOpts {
    dir: Option<String>,
    env: Option<Vec<(String, String)>>,
    extra_env: Vec<(String, String)>,
    stdin: StdinSpec,
    out_inherit: bool,
    err_inherit: bool,
}

#[derive(Default)]
enum StdinSpec {
    #[default]
    Null,
    Inherit,
    Pipe(String),
}

fn parse_opts_map(v: &Value) -> Result<SpawnOpts, String> {
    let Value::Map(m) = v else {
        return Err(format!("options must be a map, got {}", v.type_name()));
    };
    let mut opts = SpawnOpts::default();
    for (k, val) in m.iter() {
        let Some(name) = keyword_name(k) else {
            return Err(format!(
                "option keys must be keywords, got {}",
                k.type_name()
            ));
        };
        match name.as_str() {
            "dir" => opts.dir = Some(as_str(val, ":dir")?),
            "env" => opts.env = Some(env_pairs(val, ":env")?),
            "extra-env" => opts.extra_env = env_pairs(val, ":extra-env")?,
            "in" => {
                opts.stdin = match val {
                    Value::Str(s) => StdinSpec::Pipe(s.get().to_string()),
                    v if keyword_name(v).as_deref() == Some("inherit") => StdinSpec::Inherit,
                    other => {
                        return Err(format!(
                            ":in must be a string or :inherit, got {}",
                            other.type_name()
                        ));
                    }
                }
            }
            "out" => opts.out_inherit = keyword_name(val).as_deref() == Some("inherit"),
            "err" => opts.err_inherit = keyword_name(val).as_deref() == Some("inherit"),
            other => return Err(format!("unknown option :{other}")),
        }
    }
    Ok(opts)
}

// ── Spawning ──────────────────────────────────────────────────────────────────

fn build_command(argv: &[String], opts: &SpawnOpts) -> Result<Command, String> {
    let (prog, rest) = argv
        .split_first()
        .ok_or_else(|| "empty command".to_string())?;
    let mut cmd = Command::new(prog);
    cmd.args(rest);
    if let Some(dir) = &opts.dir {
        cmd.current_dir(dir);
    }
    if let Some(env) = &opts.env {
        cmd.env_clear();
        cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }
    cmd.envs(opts.extra_env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    match &opts.stdin {
        StdinSpec::Null => cmd.stdin(Stdio::null()),
        StdinSpec::Inherit => cmd.stdin(Stdio::inherit()),
        StdinSpec::Pipe(_) => cmd.stdin(Stdio::piped()),
    };
    cmd.stdout(if opts.out_inherit {
        Stdio::inherit()
    } else {
        Stdio::piped()
    });
    cmd.stderr(if opts.err_inherit {
        Stdio::inherit()
    } else {
        Stdio::piped()
    });
    Ok(cmd)
}

fn spawn_child(argv: &[String], opts: &SpawnOpts) -> Result<Child, String> {
    let mut child = build_command(argv, opts)?
        .spawn()
        .map_err(|e| format!("failed to spawn {:?}: {e}", argv[0]))?;
    if let StdinSpec::Pipe(input) = &opts.stdin {
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin
            .write_all(input.as_bytes())
            .map_err(|e| format!("failed writing stdin: {e}"))?;
        // Dropping stdin closes the pipe so the child sees EOF.
    }
    Ok(child)
}

fn wait_for(handle: &ChildProcess) -> Result<WaitResult, String> {
    if let Some(cached) = handle.result.lock().unwrap().clone() {
        return Ok(cached);
    }
    let child = handle.child.lock().unwrap().take();
    let Some(child) = child else {
        // Another thread is waiting or already reaped; spin on the cache.
        return handle
            .result
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "process is being waited on concurrently".to_string());
    };
    let output = child
        .wait_with_output()
        .map_err(|e| format!("wait failed for {}: {e}", handle.cmd))?;
    let decode = |bytes: Vec<u8>| {
        if bytes.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
    };
    let exit = output.status.code().unwrap_or(-1);
    let result = (
        exit,
        Some(decode(output.stdout).unwrap_or_default()),
        Some(decode(output.stderr).unwrap_or_default()),
    );
    *handle.result.lock().unwrap() = Some(result.clone());
    Ok(result)
}

fn handle_of(v: &Value) -> Result<GcPtr<cljrs_value::NativeObjectBox>, String> {
    match v {
        Value::NativeObject(obj) if obj.get().type_tag() == "ChildProcess" => Ok(obj.clone()),
        other => Err(format!(
            "expected a ChildProcess, got {}",
            other.type_name()
        )),
    }
}

fn with_child_process<R>(
    v: &Value,
    f: impl FnOnce(&ChildProcess) -> Result<R, String>,
) -> Result<R, String> {
    let boxed = handle_of(v)?;
    let proc = boxed
        .get()
        .downcast_ref::<ChildProcess>()
        .ok_or_else(|| "expected a ChildProcess".to_string())?;
    f(proc)
}

// ── Registration ──────────────────────────────────────────────────────────────

/// Register the `cljrs.process` namespace into `globals`. Idempotent.
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
    registry.define(
        "cljrs.process/sh",
        wrap_fn_variadic(
            "cljrs.process/sh",
            1,
            |args: &[Value]| -> Result<Value, String> {
                // clojure.java.shell/sh: leading strings, then keyword options.
                let mut argv = Vec::new();
                let mut i = 0;
                while i < args.len() {
                    match &args[i] {
                        Value::Str(s) => {
                            argv.push(s.get().to_string());
                            i += 1;
                        }
                        _ => break,
                    }
                }
                if argv.is_empty() {
                    return Err("sh needs at least one command string".to_string());
                }
                let mut opts = SpawnOpts::default();
                let mut kwargs: HashMap<String, Value> = HashMap::new();
                while i < args.len() {
                    let Some(name) = keyword_name(&args[i]) else {
                        return Err(format!(
                            "sh options must be keyword/value pairs, got {}",
                            args[i].type_name()
                        ));
                    };
                    let Some(val) = args.get(i + 1) else {
                        return Err(format!("sh option :{name} is missing a value"));
                    };
                    kwargs.insert(name, val.clone());
                    i += 2;
                }
                for (name, val) in &kwargs {
                    match name.as_str() {
                        "in" => opts.stdin = StdinSpec::Pipe(as_str(val, ":in")?),
                        "dir" => opts.dir = Some(as_str(val, ":dir")?),
                        "env" => opts.env = Some(env_pairs(val, ":env")?),
                        "extra-env" => opts.extra_env = env_pairs(val, ":extra-env")?,
                        other => return Err(format!("unknown sh option :{other}")),
                    }
                }
                let child = spawn_child(&argv, &opts)?;
                let handle = ChildProcess {
                    cmd: argv.join(" "),
                    child: Mutex::new(Some(child)),
                    result: Mutex::new(None),
                };
                let (exit, out, err) = wait_for(&handle)?;
                Ok(result_map(exit, out, err))
            },
        ),
    );

    registry.define(
        "cljrs.process/spawn",
        wrap_fn_variadic(
            "cljrs.process/spawn",
            1,
            |args: &[Value]| -> Result<Value, String> {
                let argv: Vec<String> = match &args[0] {
                    Value::Vector(v) => v
                        .get()
                        .iter()
                        .map(|e| as_str(e, "command element"))
                        .collect::<Result<_, _>>()?,
                    Value::Str(s) => vec![s.get().to_string()],
                    other => {
                        return Err(format!(
                            "spawn expects a command vector or string, got {}",
                            other.type_name()
                        ));
                    }
                };
                let opts = match args.get(1) {
                    Some(v) => parse_opts_map(v)?,
                    None => SpawnOpts::default(),
                };
                let child = spawn_child(&argv, &opts)?;
                let handle = ChildProcess {
                    cmd: argv.join(" "),
                    child: Mutex::new(Some(child)),
                    result: Mutex::new(None),
                };
                Ok(Value::NativeObject(gc_native_object(handle)))
            },
        ),
    );

    registry.define(
        "cljrs.process/wait",
        wrap_fn1("cljrs.process/wait", |v: Value| -> Result<Value, String> {
            with_child_process(&v, |proc| {
                let (exit, out, err) = wait_for(proc)?;
                Ok(result_map(exit, out, err))
            })
        }),
    );

    registry.define(
        "cljrs.process/alive?",
        wrap_fn1(
            "cljrs.process/alive?",
            |v: Value| -> Result<Value, String> {
                with_child_process(&v, |proc| {
                    let mut guard = proc.child.lock().unwrap();
                    match guard.as_mut() {
                        None => Ok(Value::Bool(false)),
                        Some(child) => match child.try_wait() {
                            Ok(None) => Ok(Value::Bool(true)),
                            Ok(Some(_)) => Ok(Value::Bool(false)),
                            Err(e) => Err(format!("try_wait failed: {e}")),
                        },
                    }
                })
            },
        ),
    );

    registry.define(
        "cljrs.process/exit-code",
        wrap_fn1(
            "cljrs.process/exit-code",
            |v: Value| -> Result<Value, String> {
                with_child_process(&v, |proc| {
                    if let Some((exit, ..)) = proc.result.lock().unwrap().clone() {
                        return Ok(Value::Long(exit as i64));
                    }
                    let mut guard = proc.child.lock().unwrap();
                    match guard.as_mut() {
                        None => Ok(Value::Nil),
                        Some(child) => match child.try_wait() {
                            Ok(Some(status)) => Ok(Value::Long(status.code().unwrap_or(-1) as i64)),
                            Ok(None) => Ok(Value::Nil),
                            Err(e) => Err(format!("try_wait failed: {e}")),
                        },
                    }
                })
            },
        ),
    );

    registry.define(
        "cljrs.process/destroy",
        wrap_fn1(
            "cljrs.process/destroy",
            |v: Value| -> Result<Value, String> {
                with_child_process(&v, |proc| {
                    let mut guard = proc.child.lock().unwrap();
                    if let Some(child) = guard.as_mut() {
                        child.kill().map_err(|e| format!("kill failed: {e}"))?;
                    }
                    Ok(Value::Nil)
                })
            },
        ),
    );

    registry.env().mark_loaded(NS);
}
