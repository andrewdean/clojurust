//! Host/system builtins for scripting: environment variables, JVM-style
//! system properties, and process exit. Registered under the `System/*`
//! static-method names (matching the `Math/*` convention) so babashka/JVM
//! Clojure code like `(System/getenv "HOME")` works unchanged.
//!
//! The restricted transaction profile denies all of these by name in
//! `cljrs_env::policy::check_native`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use cljrs_env::env::GlobalEnv;
use cljrs_gc::GcPtr;
use cljrs_value::{MapValue, PersistentList, Value, ValueError, ValueResult};

/// Bind `clojure.core/*command-line-args*` to a list of the given strings
/// (nil when empty, matching Clojure). Called by the hosting binary after the
/// standard environment is built, before user code runs.
pub fn set_command_line_args(globals: &Arc<GlobalEnv>, args: &[String]) {
    let value = if args.is_empty() {
        Value::Nil
    } else {
        Value::List(GcPtr::new(PersistentList::from_iter(
            args.iter().map(|a| Value::string(a.clone())),
        )))
    };
    if let Some(var) = globals.lookup_var("clojure.core", "*command-line-args*") {
        var.get().bind(value);
    }
}

/// `(System/getenv)` → map of all environment variables;
/// `(System/getenv "NAME")` → value string or nil.
pub(crate) fn builtin_getenv(args: &[Value]) -> ValueResult<Value> {
    match args {
        [] => {
            let mut m = MapValue::empty();
            for (k, v) in std::env::vars() {
                m = m.assoc(Value::string(k), Value::string(v));
            }
            Ok(Value::Map(m))
        }
        [Value::Str(name)] => Ok(std::env::var(name.get().as_str())
            .map(Value::string)
            .unwrap_or(Value::Nil)),
        [other] => Err(ValueError::WrongType {
            expected: "string",
            got: other.type_name().to_string(),
        }),
        _ => Err(ValueError::ArityError {
            name: "System/getenv".to_string(),
            expected: "0 or 1 args".to_string(),
            got: args.len(),
        }),
    }
}

/// `(System/exit code)` — request a clean process exit. Surfaces as the
/// uncatchable `ValueError::Exit` / `EvalError::Exit` signal, which the
/// hosting binary turns into its process exit code after unwinding.
pub(crate) fn builtin_exit(args: &[Value]) -> ValueResult<Value> {
    match args {
        [Value::Long(code)] => Err(ValueError::Exit(*code as i32)),
        [other] => Err(ValueError::WrongType {
            expected: "integer exit code",
            got: other.type_name().to_string(),
        }),
        _ => unreachable!("arity checked by registration"),
    }
}

/// Bind `clojure.core/*file*` to the running script's path and expose it as
/// the `babashka.file` system property (babashka compat). Called by the
/// hosting binary for file-backed programs; `-e`/stdin programs leave it nil.
/// The loader rebinds `*file*` around each source file it loads, so code in
/// required namespaces sees its own path, as in Clojure.
pub fn set_file(globals: &Arc<GlobalEnv>, path: &str) {
    if let Some(var) = globals.lookup_var("clojure.core", "*file*") {
        var.get().bind(Value::string(path.to_string()));
    }
    property_overrides()
        .lock()
        .unwrap()
        .insert("babashka.file".to_string(), path.to_string());
}

/// Mutable overlay written by `System/setProperty`, checked before the
/// computed defaults by `System/getProperty`.
fn property_overrides() -> &'static Mutex<HashMap<String, String>> {
    static PROPS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    PROPS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Built-in values for the JVM system properties scripts actually read.
fn default_property(name: &str) -> Option<String> {
    match name {
        "user.home" => std::env::var("HOME")
            .ok()
            .or_else(|| std::env::var("USERPROFILE").ok()),
        "user.dir" => std::env::current_dir()
            .ok()
            .map(|p| p.display().to_string()),
        "user.name" => std::env::var("USER")
            .ok()
            .or_else(|| std::env::var("USERNAME").ok()),
        "os.name" => Some(
            match std::env::consts::OS {
                "linux" => "Linux",
                "macos" => "Mac OS X",
                "windows" => "Windows",
                other => other,
            }
            .to_string(),
        ),
        "os.arch" => Some(std::env::consts::ARCH.to_string()),
        "file.separator" => Some(std::path::MAIN_SEPARATOR.to_string()),
        "path.separator" => Some(if cfg!(windows) { ";" } else { ":" }.to_string()),
        "line.separator" => Some(if cfg!(windows) { "\r\n" } else { "\n" }.to_string()),
        "java.io.tmpdir" => Some(std::env::temp_dir().display().to_string()),
        _ => None,
    }
}

/// `(System/getProperties)` — the override overlay plus computed defaults.
pub(crate) fn builtin_get_properties(_args: &[Value]) -> ValueResult<Value> {
    let mut m = MapValue::empty();
    for name in [
        "user.home",
        "user.dir",
        "user.name",
        "os.name",
        "os.arch",
        "file.separator",
        "path.separator",
        "line.separator",
        "java.io.tmpdir",
    ] {
        if let Some(v) = default_property(name) {
            m = m.assoc(Value::string(name.to_string()), Value::string(v));
        }
    }
    for (k, v) in property_overrides().lock().unwrap().iter() {
        m = m.assoc(Value::string(k.clone()), Value::string(v.clone()));
    }
    Ok(Value::Map(m))
}

/// `(System/getProperty name)` / `(System/getProperty name default)`.
pub(crate) fn builtin_get_property(args: &[Value]) -> ValueResult<Value> {
    let (name, default) = match args {
        [Value::Str(name)] => (name, None),
        [Value::Str(name), default] => (name, Some(default)),
        [other, ..] => {
            return Err(ValueError::WrongType {
                expected: "string property name",
                got: other.type_name().to_string(),
            });
        }
        _ => {
            return Err(ValueError::ArityError {
                name: "System/getProperty".to_string(),
                expected: "1 or 2 args".to_string(),
                got: args.len(),
            });
        }
    };
    if let Some(v) = property_overrides()
        .lock()
        .unwrap()
        .get(name.get().as_str())
    {
        return Ok(Value::string(v.clone()));
    }
    match default_property(name.get().as_str()) {
        Some(v) => Ok(Value::string(v)),
        None => Ok(default.cloned().unwrap_or(Value::Nil)),
    }
}

/// `(System/setProperty name value)` → previous value or nil.
pub(crate) fn builtin_set_property(args: &[Value]) -> ValueResult<Value> {
    match args {
        [Value::Str(name), Value::Str(value)] => {
            let prev = property_overrides().lock().unwrap().insert(
                name.get().as_str().to_string(),
                value.get().as_str().to_string(),
            );
            Ok(prev.map(Value::string).unwrap_or(Value::Nil))
        }
        [a, b] => Err(ValueError::WrongType {
            expected: "two strings",
            got: format!("{} and {}", a.type_name(), b.type_name()),
        }),
        _ => unreachable!("arity checked by registration"),
    }
}

// ── JVM wrapper-class statics ────────────────────────────────────────────────
// The parse/predicate statics portable .cljc libraries call in their :clj
// branches (babashka runs with the :clj feature, and so does cljrsh).

fn str_arg0<'a>(args: &'a [Value], who: &str) -> Result<&'a str, ValueError> {
    match &args[0] {
        Value::Str(s) => Ok(s.get().as_str()),
        other => Err(ValueError::Other(format!(
            "{who} expects a string, got {}",
            other.type_name()
        ))),
    }
}

pub(crate) fn builtin_long_parse(args: &[Value]) -> ValueResult<Value> {
    let s = str_arg0(args, "Long/parseLong")?;
    // Optional radix arg, like Java's (Long/parseLong s radix).
    let radix = match args.get(1) {
        None => 10,
        Some(Value::Long(r)) if (2..=36).contains(r) => *r as u32,
        Some(other) => {
            return Err(ValueError::Other(format!(
                "Long/parseLong radix must be an integer in 2..36, got {other}"
            )));
        }
    };
    i64::from_str_radix(s, radix)
        .map(Value::Long)
        .map_err(|e| ValueError::Other(format!("For input string: {s:?}: {e}")))
}

pub(crate) fn builtin_double_parse(args: &[Value]) -> ValueResult<Value> {
    let s = str_arg0(args, "Double/parseDouble")?;
    s.parse::<f64>()
        .map(Value::Double)
        .map_err(|e| ValueError::Other(format!("For input string: {s:?}: {e}")))
}

pub(crate) fn builtin_boolean_parse(args: &[Value]) -> ValueResult<Value> {
    let s = str_arg0(args, "Boolean/parseBoolean")?;
    Ok(Value::Bool(s.eq_ignore_ascii_case("true")))
}

pub(crate) fn builtin_string_value_of(args: &[Value]) -> ValueResult<Value> {
    Ok(match &args[0] {
        Value::Nil => Value::string("null".to_string()),
        Value::Str(s) => Value::string(s.get().clone()),
        other => Value::string(format!("{other}")),
    })
}

fn char_pred(args: &[Value], who: &str, f: impl Fn(char) -> bool) -> ValueResult<Value> {
    match &args[0] {
        Value::Char(c) => Ok(Value::Bool(f(*c))),
        Value::Str(s) if s.get().chars().count() == 1 => {
            Ok(Value::Bool(f(s.get().chars().next().unwrap())))
        }
        other => Err(ValueError::Other(format!(
            "{who} expects a character, got {}",
            other.type_name()
        ))),
    }
}

pub(crate) fn builtin_char_is_digit(args: &[Value]) -> ValueResult<Value> {
    char_pred(args, "Character/isDigit", |c| c.is_ascii_digit())
}

pub(crate) fn builtin_char_is_letter(args: &[Value]) -> ValueResult<Value> {
    char_pred(args, "Character/isLetter", char::is_alphabetic)
}

pub(crate) fn builtin_char_is_whitespace(args: &[Value]) -> ValueResult<Value> {
    char_pred(args, "Character/isWhitespace", char::is_whitespace)
}

pub(crate) fn builtin_current_time_millis(_args: &[Value]) -> ValueResult<Value> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| ValueError::Other(e.to_string()))?
        .as_millis() as i64;
    Ok(Value::Long(ms))
}

pub(crate) fn builtin_nano_time(_args: &[Value]) -> ValueResult<Value> {
    // Monotonic, like Java: relative to an arbitrary process-local origin.
    use std::sync::OnceLock;
    static ORIGIN: OnceLock<std::time::Instant> = OnceLock::new();
    let origin = ORIGIN.get_or_init(std::time::Instant::now);
    Ok(Value::Long(origin.elapsed().as_nanos() as i64))
}
