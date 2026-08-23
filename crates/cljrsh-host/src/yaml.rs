//! `cljrsh.yaml` — YAML parse/generate over yaml-rust2.

use cljrs_gc::GcPtr;
use cljrs_interop::{Registry, wrap_fn_variadic};
use cljrs_value::value::MapValue;
use cljrs_value::{Keyword, PersistentVector, Value};
use yaml_rust2::{Yaml, YamlEmitter, YamlLoader};

fn yaml_to_value(y: &Yaml, keywordize: bool) -> Result<Value, String> {
    Ok(match y {
        Yaml::Null | Yaml::BadValue => Value::Nil,
        Yaml::Boolean(b) => Value::Bool(*b),
        Yaml::Integer(i) => Value::Long(*i),
        Yaml::Real(r) => Value::Double(r.parse::<f64>().map_err(|e| format!("bad real: {e}"))?),
        Yaml::String(s) => Value::string(s.clone()),
        Yaml::Array(items) => Value::Vector(GcPtr::new(PersistentVector::from_iter(
            items
                .iter()
                .map(|i| yaml_to_value(i, keywordize))
                .collect::<Result<Vec<_>, _>>()?,
        ))),
        Yaml::Hash(entries) => {
            let mut m = MapValue::empty();
            for (k, v) in entries {
                let key = match k {
                    Yaml::String(s) if keywordize => Value::keyword(Keyword::parse(s.as_str())),
                    other => yaml_to_value(other, keywordize)?,
                };
                m = m.assoc(key, yaml_to_value(v, keywordize)?);
            }
            Value::Map(m)
        }
        Yaml::Alias(_) => return Err("YAML aliases are not supported".to_string()),
    })
}

fn value_to_yaml(v: &Value) -> Result<Yaml, String> {
    Ok(match v {
        Value::Nil => Yaml::Null,
        Value::Bool(b) => Yaml::Boolean(*b),
        Value::Long(n) => Yaml::Integer(*n),
        Value::Double(d) => Yaml::Real(d.to_string()),
        Value::Str(s) => Yaml::String(s.get().to_string()),
        Value::Char(c) => Yaml::String(c.to_string()),
        Value::Keyword(k) => Yaml::String(match &k.get().namespace {
            Some(ns) => format!("{ns}/{}", k.get().name),
            None => k.get().name.to_string(),
        }),
        Value::Vector(items) => Yaml::Array(
            items
                .get()
                .iter()
                .map(value_to_yaml)
                .collect::<Result<_, _>>()?,
        ),
        Value::List(items) => Yaml::Array(
            items
                .get()
                .iter()
                .map(value_to_yaml)
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(m) => {
            let mut hash = yaml_rust2::yaml::Hash::new();
            for (k, val) in m.iter() {
                hash.insert(value_to_yaml(k)?, value_to_yaml(val)?);
            }
            Yaml::Hash(hash)
        }
        other => return Err(format!("cannot encode {} as YAML", other.type_name())),
    })
}

pub fn register(registry: &mut Registry) {
    registry.define(
        "cljrsh.yaml/parse-string",
        wrap_fn_variadic(
            "cljrsh.yaml/parse-string",
            1,
            |args: &[Value]| -> Result<Value, String> {
                let Value::Str(s) = &args[0] else {
                    return Err(format!(
                        "parse-string expects a string, got {}",
                        args[0].type_name()
                    ));
                };
                // Second arg: keywordize keys (default true, clj-yaml style).
                let keywordize = args
                    .get(1)
                    .map(|v| !matches!(v, Value::Nil | Value::Bool(false)))
                    .unwrap_or(true);
                let docs = YamlLoader::load_from_str(s.get())
                    .map_err(|e| format!("YAML parse error: {e}"))?;
                match docs.len() {
                    0 => Ok(Value::Nil),
                    1 => yaml_to_value(&docs[0], keywordize),
                    _ => Ok(Value::Vector(GcPtr::new(PersistentVector::from_iter(
                        docs.iter()
                            .map(|d| yaml_to_value(d, keywordize))
                            .collect::<Result<Vec<_>, _>>()?,
                    )))),
                }
            },
        ),
    );

    registry.define(
        "cljrsh.yaml/generate-string",
        wrap_fn_variadic(
            "cljrsh.yaml/generate-string",
            1,
            |args: &[Value]| -> Result<Value, String> {
                let yaml = value_to_yaml(&args[0])?;
                let mut out = String::new();
                YamlEmitter::new(&mut out)
                    .dump(&yaml)
                    .map_err(|e| format!("YAML generate error: {e}"))?;
                // yaml-rust2 emits a leading "---\n" document marker; clj-yaml
                // does not — strip it for compatibility.
                let out = out.strip_prefix("---\n").unwrap_or(&out).to_string();
                Ok(Value::string(format!("{out}\n")))
            },
        ),
    );
}
