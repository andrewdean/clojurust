//! babashka pod protocol client (the `cljrsh.pods` namespace and the
//! `babashka.pods` compat veneer).
//!
//! `(load-pod "./my-pod")` spawns the executable with `BABASHKA_POD=true`,
//! speaks bencode over its stdio (via `cljrs-bencode`), and registers every
//! namespace the pod's `describe` reply declares: plain vars become native
//! fns that do a synchronous `invoke` round-trip (payloads EDN or JSON per
//! the pod's declared format), and `"code"` vars are evaluated client-side
//! in the pod's namespace.
//!
//! Threading follows the `cljrs-nrepl` split: `GcPtr`s never leave the
//! interpreter thread — a dedicated OS reader thread decodes bencode into
//! plain `Send` structs and hands them over an mpsc channel; payload strings
//! are parsed into `Value`s on the interpreter side. `out`/`err` replies are
//! forwarded to this process's stdout/stderr as they arrive.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use cljrs_bencode::{Bencode, decode, encode_to_vec};
use cljrs_env::env::{Env, GlobalEnv};
use cljrs_gc::{GcPtr, MarkVisitor, Trace};
use cljrs_interop::{Registry, wrap_fn_variadic};
use cljrs_value::{ExceptionInfo, NativeObject, Value, ValueError, gc_native_object};

pub mod transit;

pub const NS: &str = "cljrsh.pods";

const DESCRIBE_TIMEOUT: Duration = Duration::from_secs(10);

// ── Wire messages (Send; produced by the reader thread) ──────────────────────

#[derive(Debug)]
struct VarSpec {
    name: String,
    code: Option<String>,
}

#[derive(Debug)]
enum Msg {
    Describe {
        format: String,
        namespaces: Vec<(String, Vec<VarSpec>)>,
    },
    Reply {
        value: Option<String>,
        out: Option<String>,
        err: Option<String>,
        ex_message: Option<String>,
        ex_data: Option<String>,
        done: bool,
        error: bool,
    },
}

fn get<'a>(d: &'a Bencode, key: &str) -> Option<&'a Bencode> {
    d.as_dict()?.get(key.as_bytes())
}

fn decode_msg(msg: &Bencode) -> Option<Msg> {
    if get(msg, "format").is_some() || get(msg, "namespaces").is_some() {
        let format = get(msg, "format")
            .and_then(|v| v.as_str())
            .unwrap_or("edn")
            .to_string();
        let mut namespaces = Vec::new();
        if let Some(Bencode::List(nss)) = get(msg, "namespaces") {
            for ns in nss {
                let name = get(ns, "name")?.as_str()?.to_string();
                let mut vars = Vec::new();
                if let Some(Bencode::List(vs)) = get(ns, "vars") {
                    for v in vs {
                        vars.push(VarSpec {
                            name: get(v, "name")?.as_str()?.to_string(),
                            code: get(v, "code").and_then(|c| c.as_str()).map(str::to_string),
                        });
                    }
                }
                namespaces.push((name, vars));
            }
        }
        return Some(Msg::Describe { format, namespaces });
    }
    let status: Vec<String> = match get(msg, "status") {
        Some(Bencode::List(items)) => items
            .iter()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    Some(Msg::Reply {
        value: get(msg, "value")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        out: get(msg, "out").and_then(|v| v.as_str()).map(str::to_string),
        err: get(msg, "err").and_then(|v| v.as_str()).map(str::to_string),
        ex_message: get(msg, "ex-message")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        ex_data: get(msg, "ex-data")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        done: status.iter().any(|s| s == "done"),
        error: status.iter().any(|s| s == "error"),
    })
}

// ── The pod handle ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
enum PayloadFormat {
    Edn,
    Json,
    TransitJson,
}

pub struct Pod {
    name: String,
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    replies: Mutex<mpsc::Receiver<Msg>>,
    counter: AtomicU64,
    format: Mutex<PayloadFormat>,
}

impl std::fmt::Debug for Pod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pod {{ name: {:?} }}", self.name)
    }
}

impl Trace for Pod {
    fn trace(&self, _visitor: &mut MarkVisitor) {}
}

impl NativeObject for Pod {
    fn type_tag(&self) -> &str {
        "Pod"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Debug)]
enum PodError {
    Io(String),
    /// The pod reported an error reply: (ex-message, ex-data EDN string).
    Ex(String, Option<String>),
}

impl Pod {
    fn send(&self, msg: &Bencode) -> Result<(), PodError> {
        let bytes = encode_to_vec(msg);
        let mut stdin = self.stdin.lock().unwrap();
        let Some(w) = stdin.as_mut() else {
            return Err(PodError::Io(format!("pod {} is shut down", self.name)));
        };
        w.write_all(&bytes)
            .and_then(|_| w.flush())
            .map_err(|e| PodError::Io(format!("writing to pod {}: {e}", self.name)))
    }

    fn dict(entries: Vec<(&str, Bencode)>) -> Bencode {
        let mut m = BTreeMap::new();
        for (k, v) in entries {
            m.insert(k.as_bytes().to_vec(), v);
        }
        Bencode::Dict(m)
    }

    /// Synchronous invoke: send, then pump replies (forwarding out/err) until
    /// `done`. Returns the last payload string, if any.
    fn invoke(&self, var: &str, args_payload: String) -> Result<Option<String>, PodError> {
        let id = self.counter.fetch_add(1, Ordering::Relaxed).to_string();
        self.send(&Self::dict(vec![
            ("op", Bencode::str("invoke")),
            ("id", Bencode::str(&id)),
            ("var", Bencode::str(var)),
            ("args", Bencode::str(&args_payload)),
        ]))?;
        let replies = self.replies.lock().unwrap();
        let mut last_value = None;
        loop {
            let msg = replies
                .recv()
                .map_err(|_| PodError::Io(format!("pod {} closed its output", self.name)))?;
            let Msg::Reply {
                value,
                out,
                err,
                ex_message,
                ex_data,
                done,
                error,
            } = msg
            else {
                continue; // stray describe — ignore
            };
            if let Some(text) = out {
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            if let Some(text) = err {
                eprint!("{text}");
            }
            if let Some(v) = value {
                last_value = Some(v);
            }
            if error {
                return Err(PodError::Ex(
                    ex_message.unwrap_or_else(|| "pod error".to_string()),
                    ex_data,
                ));
            }
            if done {
                return Ok(last_value);
            }
        }
    }

    fn shutdown(&self) {
        let _ = self.send(&Self::dict(vec![("op", Bencode::str("shutdown"))]));
        let mut child = self.child.lock().unwrap();
        // Short grace for pods that exit on the shutdown op, then kill — like
        // babashka, which destroys the process right after sending shutdown.
        // Never close stdin while the pod lives: common Go pods panic (nil
        // Message deref) in their read-error path on EOF, spraying stderr.
        for _ in 0..20 {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for Pod {
    fn drop(&mut self) {
        // Best-effort: don't leave pod processes behind when the handle dies.
        if let Ok(Some(_)) = self.child.lock().unwrap().try_wait() {
            return;
        }
        self.shutdown();
    }
}

// ── Spawning + registration ───────────────────────────────────────────────────

fn spawn_pod(argv: &[String]) -> Result<Arc<Pod>, PodError> {
    let (prog, rest) = argv
        .split_first()
        .ok_or_else(|| PodError::Io("empty pod command".to_string()))?;
    let mut child = Command::new(prog)
        .args(rest)
        .env("BABASHKA_POD", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| PodError::Io(format!("failed to spawn pod {prog:?}: {e}")))?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");

    let (tx, rx) = mpsc::channel();
    let reader_name = prog.clone();
    std::thread::Builder::new()
        .name(format!("pod-reader:{reader_name}"))
        .spawn(move || reader_loop(stdout, tx))
        .map_err(|e| PodError::Io(format!("failed to spawn pod reader: {e}")))?;

    Ok(Arc::new(Pod {
        name: prog.clone(),
        child: Mutex::new(child),
        stdin: Mutex::new(Some(stdin)),
        replies: Mutex::new(rx),
        counter: AtomicU64::new(1),
        format: Mutex::new(PayloadFormat::Edn),
    }))
}

fn reader_loop(mut stdout: impl Read, tx: mpsc::Sender<Msg>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match decode(&buf) {
            Ok(Some((msg, used))) => {
                buf.drain(..used);
                if let Some(m) = decode_msg(&msg)
                    && tx.send(m).is_err()
                {
                    return; // client gone
                }
            }
            Ok(None) => match stdout.read(&mut chunk) {
                Ok(0) | Err(_) => return, // pod exited
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            },
            Err(_) => return, // malformed stream; give up
        }
    }
}

/// Encode already-evaluated call args as the pod's payload (a serialized
/// vector). EDN uses the values' readable printing; JSON goes through the
/// shared converter.
fn encode_args(format: PayloadFormat, args: &[Value]) -> Result<String, String> {
    match format {
        PayloadFormat::Edn => {
            let items: Vec<String> = args.iter().map(|v| format!("{v}")).collect();
            Ok(format!("[{}]", items.join(" ")))
        }
        PayloadFormat::Json => {
            let items: Vec<serde_json_value> = args
                .iter()
                .map(cljrsh_host::json::value_to_json)
                .collect::<Result<_, _>>()?;
            serde_json::to_string(&items).map_err(|e| e.to_string())
        }
        PayloadFormat::TransitJson => {
            // babashka sends invoke args as a seq, i.e. a transit LIST.
            let items: Vec<serde_json_value> =
                args.iter().map(transit::encode).collect::<Result<_, _>>()?;
            let list = serde_json_value::Array(vec![
                serde_json_value::String("~#list".to_string()),
                serde_json_value::Array(items),
            ]);
            serde_json::to_string(&list).map_err(|e| e.to_string())
        }
    }
}

use serde_json::Value as serde_json_value;

fn parse_payload(format: PayloadFormat, payload: &str) -> Result<Value, String> {
    match format {
        PayloadFormat::Edn => {
            let mut parser = cljrs_reader::Parser::new(payload.to_string(), "<pod>".to_string());
            match parser.parse_one() {
                Ok(Some(form)) => Ok(cljrs_builtins::form::form_to_value(&form)),
                Ok(None) => Ok(Value::Nil),
                Err(e) => Err(format!("bad pod EDN payload: {e}")),
            }
        }
        PayloadFormat::Json => {
            let parsed: serde_json_value =
                serde_json::from_str(payload).map_err(|e| format!("bad pod JSON payload: {e}"))?;
            Ok(cljrsh_host::json::json_to_value(&parsed, true))
        }
        PayloadFormat::TransitJson => {
            let parsed: serde_json_value = serde_json::from_str(payload)
                .map_err(|e| format!("bad pod transit payload: {e}"))?;
            transit::decode_with(&parsed, &registered_tag_handler)
        }
    }
}

fn pod_ex(message: String, ex_data: Option<String>) -> ValueError {
    let data = ex_data
        .as_deref()
        .and_then(|edn| parse_payload(PayloadFormat::Edn, edn).ok())
        .and_then(|v| match v {
            Value::Map(m) => Some(m),
            _ => None,
        });
    ValueError::Thrown(Value::Error(GcPtr::new(ExceptionInfo::new(
        ValueError::Other(message.clone()),
        message,
        data,
        None,
    ))))
}

fn invoke_to_value(pod: &Pod, var: &str, args: &[Value]) -> Result<Value, ValueError> {
    let format = *pod.format.lock().unwrap();
    let payload = encode_args(format, args).map_err(ValueError::Other)?;
    match pod.invoke(var, payload) {
        Ok(Some(v)) => parse_payload(format, &v).map_err(ValueError::Other),
        Ok(None) => Ok(Value::Nil),
        Err(PodError::Io(e)) => Err(ValueError::Other(e)),
        Err(PodError::Ex(msg, data)) => Err(pod_ex(msg, data)),
    }
}

/// Every live pod, so the hosting binary can shut them down cleanly at
/// process exit (Drop alone is skipped by std::process::exit).
fn live_pods() -> &'static Mutex<Vec<std::sync::Weak<Pod>>> {
    static PODS: std::sync::OnceLock<Mutex<Vec<std::sync::Weak<Pod>>>> = std::sync::OnceLock::new();
    PODS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Send `shutdown` to every still-running pod and wait briefly. The hosting
/// binary calls this once before exiting.
pub fn shutdown_all() {
    let pods: Vec<Arc<Pod>> = live_pods()
        .lock()
        .unwrap()
        .drain(..)
        .filter_map(|w| w.upgrade())
        .collect();
    for pod in pods {
        pod.shutdown();
    }
}

/// Load a pod and register its namespaces. Returns the pod handle.
/// Load a pod executable by path — the binary uses this for bb.edn `:pods`
/// entries after registry resolution.
pub fn load_registry_pod(globals: &Arc<GlobalEnv>, exe: &str) -> Result<Value, String> {
    load_pod(globals, &[exe.to_string()])
}

fn load_pod(globals: &Arc<GlobalEnv>, argv: &[String]) -> Result<Value, String> {
    let pod = spawn_pod(argv).map_err(|e| match e {
        PodError::Io(m) => m,
        PodError::Ex(m, _) => m,
    })?;

    pod.send(&Pod::dict(vec![("op", Bencode::str("describe"))]))
        .map_err(|e| match e {
            PodError::Io(m) => m,
            PodError::Ex(m, _) => m,
        })?;
    let described = {
        let replies = pod.replies.lock().unwrap();
        loop {
            match replies.recv_timeout(DESCRIBE_TIMEOUT) {
                Ok(Msg::Describe { format, namespaces }) => break (format, namespaces),
                Ok(Msg::Reply { .. }) => continue,
                Err(_) => {
                    return Err(format!(
                        "pod {} did not answer describe within {DESCRIBE_TIMEOUT:?}",
                        pod.name
                    ));
                }
            }
        }
    };
    let (format, namespaces) = described;
    *pod.format.lock().unwrap() = match format.as_str() {
        "json" => PayloadFormat::Json,
        "edn" => PayloadFormat::Edn,
        "transit+json" => PayloadFormat::TransitJson,
        other => {
            return Err(format!(
                "pod {} uses unsupported format {other:?}",
                pod.name
            ));
        }
    };

    for (ns_name, vars) in &namespaces {
        globals.get_or_create_ns(ns_name);
        globals.refer_all(ns_name, "clojure.core");
        let mut code_vars: Vec<&VarSpec> = Vec::new();
        for var in vars {
            if var.code.is_some() {
                code_vars.push(var);
                continue;
            }
            let qualified = format!("{ns_name}/{}", var.name);
            let pod_ref = pod.clone();
            let target = qualified.clone();
            // NativeFn::with_closure keeps the ValueError intact (a pod error
            // must surface as a real ex-info, not a stringified message).
            let nf = cljrs_value::NativeFn::with_closure(
                qualified.clone(),
                cljrs_value::Arity::Variadic { min: 0 },
                move |args: &[Value]| invoke_to_value(&pod_ref, &target, args),
            );
            globals.intern(
                ns_name,
                Arc::from(var.name.as_str()),
                Value::NativeFunction(GcPtr::new(nf)),
            );
        }
        // "code" vars: client-side Clojure evaluated in the pod namespace,
        // after the native stubs exist (the code typically calls them).
        for var in code_vars {
            let code = var.code.as_deref().unwrap();
            let mut env = Env::new(globals.clone(), ns_name);
            let mut parser =
                cljrs_reader::Parser::new(code.to_string(), format!("<pod:{ns_name}>"));
            let forms = parser
                .parse_all()
                .map_err(|e| format!("pod {} code var {}: {e}", pod.name, var.name))?;
            for form in forms {
                let _frame = cljrs_gc::push_alloc_frame();
                cljrs_interp::eval::eval(&form, &mut env)
                    .map_err(|e| format!("pod {} code var {}: {e}", pod.name, var.name))?;
            }
        }
        globals.mark_loaded(ns_name);
    }

    live_pods().lock().unwrap().push(Arc::downgrade(&pod));
    Ok(Value::NativeObject(gc_native_object(PodHandle(pod))))
}

/// The GC-managed wrapper the script holds; dropping it shuts the pod down.
#[derive(Debug)]
pub struct PodHandle(Arc<Pod>);

impl Trace for PodHandle {
    fn trace(&self, _visitor: &mut MarkVisitor) {}
}

impl NativeObject for PodHandle {
    fn type_tag(&self) -> &str {
        "Pod"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ── Registration ──────────────────────────────────────────────────────────────

/// Unknown-tag resolver backed by the Clojure-side registry
/// `babashka.pods/transit-read-handlers` (an atom of tag-string → fn),
/// invoked on the interpreter thread via the eval-context callback.
fn registered_tag_handler(tag: &str, rep: Value) -> Option<Value> {
    let (globals, _) = cljrs_env::callback::capture_eval_context()?;
    let var = globals.lookup_var("babashka.pods", "transit-read-handlers")?;
    let atom = cljrs_env::dynamics::deref_var(&var)?;
    let Value::Atom(a) = atom else { return None };
    let Value::Map(handlers) = a.get().deref() else {
        return None;
    };
    let f = handlers.get(&Value::string(tag.to_string()))?;
    cljrs_env::callback::invoke(&f, vec![rep]).ok()
}

const BABASHKA_PODS_SOURCE: &str = r#"
;; babashka.pods compatibility veneer over cljrsh.pods.
(ns babashka.pods
  (:require [cljrsh.pods]))
(def load-pod cljrsh.pods/load-pod)
(def unload-pod cljrsh.pods/unload-pod)

;; Custom transit tag handlers, consulted by the pod payload decoder for
;; unknown ~#tags. Keyed by tag string; the handler receives the decoded rep.
(def transit-read-handlers (atom {}))

(defn add-transit-read-handler! [tag f]
  (swap! transit-read-handlers assoc (str tag) f)
  nil)

(defn add-transit-write-handler!
  "Accepted for compatibility; cljrsh's transit encoder writes plain data
  only, so custom write handlers are ignored."
  [& _]
  nil)

(defn set-default-transit-write-handler! [& _] nil)
"#;

/// Register `cljrsh.pods` and the `babashka.pods` veneer. Idempotent.
pub fn init(globals: &Arc<GlobalEnv>) {
    if globals.is_loaded(NS) {
        return;
    }
    globals.get_or_create_ns(NS);
    globals.refer_all(NS, "clojure.core");
    let registry = Registry::for_require(globals.clone());

    registry.define(
        "cljrsh.pods/load-pod",
        wrap_fn_variadic(
            "cljrsh.pods/load-pod",
            1,
            |args: &[Value]| -> Result<Value, String> {
                // GlobalEnv holds GcPtrs (not Send), so it cannot be captured
                // by a native-fn closure; recover it from the thread-local
                // eval context instead (pushed around every native call).
                let (g, _ns) = cljrs_env::callback::capture_eval_context()
                    .ok_or_else(|| "load-pod called outside an eval context".to_string())?;
                let argv: Vec<String> = match &args[0] {
                    Value::Str(s) => vec![s.get().to_string()],
                    Value::Vector(v) => v
                        .get()
                        .iter()
                        .map(|e| match &e {
                            Value::Str(s) => Ok(s.get().to_string()),
                            other => Err(format!(
                                "pod command elements must be strings, got {}",
                                other.type_name()
                            )),
                        })
                        .collect::<Result<_, _>>()?,
                    // (load-pod 'org.babashka/foo "0.1.0"): resolve from the
                    // babashka pod registry, downloading on first use.
                    Value::Symbol(sym) => {
                        let name = sym.get().to_string();
                        let version = match args.get(1) {
                            Some(Value::Str(s)) => s.get().to_string(),
                            Some(Value::Map(m)) => match m
                                .get(&Value::keyword(cljrs_value::Keyword::simple("version")))
                            {
                                Some(Value::Str(s)) => s.get().to_string(),
                                _ => {
                                    return Err(format!(
                                        "load-pod {name}: opts map needs a :version string"
                                    ));
                                }
                            },
                            _ => {
                                return Err(format!(
                                    "load-pod {name}: registry pods need a version,                                      e.g. (load-pod '{name} \"0.1.0\")"
                                ));
                            }
                        };
                        let exe = cljrsh_project::pods::ensure_registry_pod(
                            &name,
                            &version,
                            &cljrsh_project::pods::default_cache_dir(),
                        )?;
                        vec![exe.to_string_lossy().into_owned()]
                    }
                    other => {
                        return Err(format!(
                            "load-pod expects a path string, command vector, or                              registry symbol, got {}",
                            other.type_name()
                        ));
                    }
                };
                load_pod(&g, &argv)
            },
        ),
    );

    registry.define(
        "cljrsh.pods/unload-pod",
        wrap_fn_variadic(
            "cljrsh.pods/unload-pod",
            1,
            |args: &[Value]| -> Result<Value, String> {
                match &args[0] {
                    Value::NativeObject(obj) => {
                        if let Some(handle) = obj.get().downcast_ref::<PodHandle>() {
                            handle.0.shutdown();
                            Ok(Value::Nil)
                        } else {
                            Err("unload-pod expects a pod handle".to_string())
                        }
                    }
                    other => Err(format!(
                        "unload-pod expects a pod handle, got {}",
                        other.type_name()
                    )),
                }
            },
        ),
    );

    registry.env().mark_loaded(NS);
    globals.register_builtin_source("babashka.pods", BABASHKA_PODS_SOURCE);
}
