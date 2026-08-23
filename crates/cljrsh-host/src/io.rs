//! `cljrsh.io` — stdin access and EDN parsing natives backing the binary's
//! `-i`/`-I`/`--stream` flags and the `*input*` convention.

use cljrs_builtins::form::form_to_value;
use cljrs_gc::GcPtr;
use cljrs_interop::{Registry, wrap_fn1, wrap_fn_variadic};
use cljrs_value::{PersistentVector, Value};

/// Read one line from stdin (newline stripped); `None` at EOF.
/// Shared by the native fn and the binary's `--stream` loop.
pub fn read_line() -> Option<String> {
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) => None,
        Ok(_) => {
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            Some(line)
        }
        Err(_) => None,
    }
}

/// Read stdin to EOF.
pub fn read_all() -> std::io::Result<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

/// Parse every form in `src` as data (no evaluation) into values.
pub fn read_edn_all(src: &str, origin: &str) -> Result<Vec<Value>, String> {
    let mut parser = cljrs_reader::Parser::new(src.to_string(), origin.to_string());
    let forms = parser.parse_all().map_err(|e| format!("EDN parse error: {e}"))?;
    Ok(forms.iter().map(form_to_value).collect())
}

/// Parse the first form in `src` as data; `None` for blank input.
pub fn read_edn_one(src: &str, origin: &str) -> Result<Option<Value>, String> {
    let mut parser = cljrs_reader::Parser::new(src.to_string(), origin.to_string());
    match parser.parse_one() {
        Ok(Some(form)) => Ok(Some(form_to_value(&form))),
        Ok(None) => Ok(None),
        Err(e) => Err(format!("EDN parse error: {e}")),
    }
}

pub fn register(registry: &mut Registry) {
    registry.define(
        "cljrsh.io/stdin-read-line",
        wrap_fn_variadic(
            "cljrsh.io/stdin-read-line",
            0,
            |_args: &[Value]| -> Result<Value, String> {
                Ok(read_line().map(Value::string).unwrap_or(Value::Nil))
            },
        ),
    );
    registry.define(
        "cljrsh.io/stdin-read-all",
        wrap_fn_variadic(
            "cljrsh.io/stdin-read-all",
            0,
            |_args: &[Value]| -> Result<Value, String> {
                read_all().map(Value::string).map_err(|e| e.to_string())
            },
        ),
    );
    registry.define(
        "cljrsh.io/read-edn-string",
        wrap_fn1(
            "cljrsh.io/read-edn-string",
            |v: Value| -> Result<Value, String> {
                let Value::Str(s) = &v else {
                    return Err(format!("expected a string, got {}", v.type_name()));
                };
                read_edn_one(s.get(), "<edn>").map(|o| o.unwrap_or(Value::Nil))
            },
        ),
    );
    registry.define(
        "cljrsh.io/read-edn-all",
        wrap_fn1(
            "cljrsh.io/read-edn-all",
            |v: Value| -> Result<Value, String> {
                let Value::Str(s) = &v else {
                    return Err(format!("expected a string, got {}", v.type_name()));
                };
                let values = read_edn_all(s.get(), "<edn>")?;
                Ok(Value::Vector(GcPtr::new(PersistentVector::from_iter(values))))
            },
        ),
    );
}
