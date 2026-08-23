//! `cljrsh.json` — JSON parse/generate over serde_json.
//!
//! `(parse-string s)` → string keys; `(parse-string s true)` or
//! `(parse-string s {:key-fn keyword})`-style truthy second arg → keyword
//! keys (the cheshire convention the compat layer forwards).

use cljrs_gc::GcPtr;
use cljrs_interop::{Registry, wrap_fn_variadic};
use cljrs_value::value::MapValue;
use cljrs_value::{Keyword, PersistentVector, Value};

pub fn register(registry: &mut Registry) {
    registry.define(
        "cljrsh.json/parse-string",
        wrap_fn_variadic(
            "cljrsh.json/parse-string",
            1,
            |args: &[Value]| -> Result<Value, String> {
                let src = match &args[0] {
                    Value::Str(s) => s.get().clone(),
                    other => {
                        return Err(format!(
                            "parse-string expects a string, got {}",
                            other.type_name()
                        ));
                    }
                };
                let keywordize = args
                    .get(1)
                    .map(|v| !matches!(v, Value::Nil | Value::Bool(false)))
                    .unwrap_or(false);
                let parsed: serde_json::Value =
                    serde_json::from_str(&src).map_err(|e| format!("JSON parse error: {e}"))?;
                Ok(json_to_value(&parsed, keywordize))
            },
        ),
    );

    registry.define(
        "cljrsh.json/generate-string",
        wrap_fn_variadic(
            "cljrsh.json/generate-string",
            1,
            |args: &[Value]| -> Result<Value, String> {
                let pretty = args
                    .get(1)
                    .map(pretty_opt)
                    .unwrap_or(false);
                let json = value_to_json(&args[0])?;
                let out = if pretty {
                    serde_json::to_string_pretty(&json)
                } else {
                    serde_json::to_string(&json)
                }
                .map_err(|e| format!("JSON generate error: {e}"))?;
                Ok(Value::string(out))
            },
        ),
    );
}

/// Second arg to generate-string: `{:pretty true}` (cheshire style).
fn pretty_opt(v: &Value) -> bool {
    if let Value::Map(m) = v
        && let Some(Value::Bool(true)) = m.get(&Value::keyword(Keyword::simple("pretty")))
    {
        return true;
    }
    false
}

pub fn json_to_value(json: &serde_json::Value, keywordize: bool) -> Value {
    match json {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Long(i)
            } else {
                Value::Double(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        serde_json::Value::String(s) => Value::string(s.clone()),
        serde_json::Value::Array(items) => Value::Vector(GcPtr::new(
            PersistentVector::from_iter(items.iter().map(|i| json_to_value(i, keywordize))),
        )),
        serde_json::Value::Object(entries) => {
            let mut m = MapValue::empty();
            for (k, v) in entries {
                let key = if keywordize {
                    Value::keyword(Keyword::parse(k.as_str()))
                } else {
                    Value::string(k.clone())
                };
                m = m.assoc(key, json_to_value(v, keywordize));
            }
            Value::Map(m)
        }
    }
}

pub fn value_to_json(v: &Value) -> Result<serde_json::Value, String> {
    Ok(match v {
        Value::Nil => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Long(n) => serde_json::Value::from(*n),
        Value::Double(d) => {
            if d.is_finite() {
                serde_json::Value::from(*d)
            } else {
                return Err(format!("cannot encode non-finite double {d} as JSON"));
            }
        }
        Value::Str(s) => serde_json::Value::String(s.get().to_string()),
        Value::Char(c) => serde_json::Value::String(c.to_string()),
        Value::Keyword(k) => serde_json::Value::String(qualified_name(
            k.get().namespace.as_deref(),
            &k.get().name,
        )),
        Value::Symbol(s) => serde_json::Value::String(qualified_name(
            s.get().namespace.as_deref(),
            &s.get().name,
        )),
        Value::Uuid(u) => serde_json::Value::String(uuid::Uuid::from_u128(*u).to_string()),
        Value::Vector(items) => serde_json::Value::Array(
            items
                .get()
                .iter()
                .map(value_to_json)
                .collect::<Result<_, _>>()?,
        ),
        Value::List(items) => serde_json::Value::Array(
            items
                .get()
                .iter()
                .map(value_to_json)
                .collect::<Result<_, _>>()?,
        ),
        Value::Set(set) => serde_json::Value::Array(
            set.iter()
                .map(value_to_json)
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(m) => {
            let mut obj = serde_json::Map::new();
            for (k, val) in m.iter() {
                let key = match &k {
                    Value::Str(s) => s.get().to_string(),
                    Value::Keyword(kw) => {
                        qualified_name(kw.get().namespace.as_deref(), &kw.get().name)
                    }
                    Value::Symbol(s) => {
                        qualified_name(s.get().namespace.as_deref(), &s.get().name)
                    }
                    Value::Long(n) => n.to_string(),
                    other => {
                        return Err(format!(
                            "cannot encode map key of type {} as JSON",
                            other.type_name()
                        ));
                    }
                };
                obj.insert(key, value_to_json(val)?);
            }
            serde_json::Value::Object(obj)
        }
        other => {
            return Err(format!(
                "cannot encode {} as JSON",
                other.type_name()
            ));
        }
    })
}

fn qualified_name(ns: Option<&str>, name: &str) -> String {
    match ns {
        Some(ns) => format!("{ns}/{name}"),
        None => name.to_string(),
    }
}
