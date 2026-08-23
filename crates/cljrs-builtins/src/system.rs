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
    if let Some(v) = property_overrides().lock().unwrap().get(name.get().as_str()) {
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
            let prev = property_overrides()
                .lock()
                .unwrap()
                .insert(name.get().as_str().to_string(), value.get().as_str().to_string());
            Ok(prev.map(Value::string).unwrap_or(Value::Nil))
        }
        [a, b] => Err(ValueError::WrongType {
            expected: "two strings",
            got: format!("{} and {}", a.type_name(), b.type_name()),
        }),
        _ => unreachable!("arity checked by registration"),
    }
}
