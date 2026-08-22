//! Bidirectional value conversion: nu `Value` ↔ clj.rs `Value`.
//!
//! Conventions (see the cljrsh plan): record keys → keywords (configurable),
//! tables → vectors of maps, Filesize → bytes as Long, Duration → nanoseconds
//! as Long, Date → RFC3339 string (until the runtime's #inst Instant lands —
//! single seam in `date_to_value`), Binary ↔ byte array. Closures, custom
//! values, and unbounded ranges do not convert and raise errors.

use cljrs_gc::GcPtr;
use cljrs_value::value::MapValue;
use cljrs_value::{Keyword, PersistentVector, Value};
use nu_protocol::{Record, Span, Value as NuValue};

/// nu dates become `#inst` instants (epoch milliseconds, UTC).
fn date_to_value(dt: &chrono::DateTime<chrono::FixedOffset>) -> Value {
    Value::Instant(dt.timestamp_millis())
}

pub fn nu_to_clj(v: &NuValue, keywordize: bool) -> Result<Value, String> {
    Ok(match v {
        NuValue::Nothing { .. } => Value::Nil,
        NuValue::Bool { val, .. } => Value::Bool(*val),
        NuValue::Int { val, .. } => Value::Long(*val),
        NuValue::Float { val, .. } => Value::Double(*val),
        NuValue::String { val, .. } | NuValue::Glob { val, .. } => Value::string(val.clone()),
        NuValue::Filesize { val, .. } => Value::Long(val.get()),
        NuValue::Duration { val, .. } => Value::Long(*val),
        NuValue::Date { val, .. } => date_to_value(val),
        NuValue::Binary { val, .. } => Value::ByteArray(GcPtr::new(std::sync::Mutex::new(
            val.iter().map(|&b| b as i8).collect(),
        ))),
        NuValue::Record { val, .. } => record_to_map(val, keywordize)?,
        NuValue::List { vals, .. } => Value::Vector(GcPtr::new(PersistentVector::from_iter(
            vals.iter()
                .map(|v| nu_to_clj(v, keywordize))
                .collect::<Result<Vec<_>, _>>()?,
        ))),
        NuValue::Range { val, .. } => {
            // Realize bounded ranges; refuse unbounded ones.
            match val.as_ref() {
                nu_protocol::Range::IntRange(r) => {
                    if matches!(r.end(), std::ops::Bound::Unbounded) {
                        return Err("cannot convert an unbounded nu range".to_string());
                    }
                    let items: Vec<Value> = r
                        .into_range_iter(nu_protocol::Signals::EMPTY)
                        .map(Value::Long)
                        .collect();
                    Value::Vector(GcPtr::new(PersistentVector::from_iter(items)))
                }
                nu_protocol::Range::FloatRange(r) => {
                    if matches!(r.end(), std::ops::Bound::Unbounded) {
                        return Err("cannot convert an unbounded nu range".to_string());
                    }
                    let items: Vec<Value> = r
                        .into_range_iter(nu_protocol::Signals::EMPTY)
                        .map(Value::Double)
                        .collect();
                    Value::Vector(GcPtr::new(PersistentVector::from_iter(items)))
                }
            }
        }
        NuValue::CellPath { val, .. } => Value::string(val.to_string()),
        NuValue::Error { error, .. } => {
            return Err(format!("nu pipeline produced an error value: {error}"));
        }
        NuValue::Closure { .. } => {
            return Err("cannot convert a nu closure to a Clojure value".to_string());
        }
        NuValue::Custom { val, .. } => {
            return Err(format!(
                "cannot convert nu custom value ({}) to a Clojure value",
                val.type_name()
            ));
        }
    })
}

fn record_to_map(record: &Record, keywordize: bool) -> Result<Value, String> {
    let mut m = MapValue::empty();
    for (k, v) in record.iter() {
        let key = if keywordize {
            Value::keyword(Keyword::simple(k.as_str()))
        } else {
            Value::string(k.to_string())
        };
        m = m.assoc(key, nu_to_clj(v, keywordize)?);
    }
    Ok(Value::Map(m))
}

fn qualified(ns: &Option<std::sync::Arc<str>>, name: &str) -> String {
    match ns {
        Some(ns) => format!("{ns}/{name}"),
        None => name.to_string(),
    }
}

pub fn clj_to_nu(v: &Value) -> Result<NuValue, String> {
    let span = Span::unknown();
    Ok(match v {
        Value::Nil => NuValue::nothing(span),
        Value::Bool(b) => NuValue::bool(*b, span),
        Value::Long(n) => NuValue::int(*n, span),
        Value::Double(d) => NuValue::float(*d, span),
        Value::Str(s) => NuValue::string(s.get().to_string(), span),
        Value::Char(c) => NuValue::string(c.to_string(), span),
        Value::Keyword(k) => NuValue::string(qualified(&k.get().namespace, &k.get().name), span),
        Value::Symbol(s) => NuValue::string(qualified(&s.get().namespace, &s.get().name), span),
        Value::Uuid(u) => NuValue::string(uuid::Uuid::from_u128(*u).to_string(), span),
        Value::Instant(ms) => NuValue::date(
            chrono::DateTime::from_timestamp_millis(*ms)
                .ok_or_else(|| format!("instant out of range: {ms}"))?
                .fixed_offset(),
            span,
        ),
        Value::ByteArray(bytes) => NuValue::binary(
            bytes
                .get()
                .lock()
                .unwrap()
                .iter()
                .map(|&b| b as u8)
                .collect::<Vec<u8>>(),
            span,
        ),
        Value::Vector(items) => NuValue::list(
            items
                .get()
                .iter()
                .map(clj_to_nu)
                .collect::<Result<Vec<_>, _>>()?,
            span,
        ),
        Value::List(items) => NuValue::list(
            items
                .get()
                .iter()
                .map(clj_to_nu)
                .collect::<Result<Vec<_>, _>>()?,
            span,
        ),
        Value::Set(set) => {
            let items: Vec<NuValue> = set.iter().map(clj_to_nu).collect::<Result<_, _>>()?;
            NuValue::list(items, span)
        }
        Value::Map(m) => {
            let mut record = Record::new();
            for (k, val) in m.iter() {
                let key = match &k {
                    Value::Str(s) => s.get().to_string(),
                    Value::Keyword(kw) => qualified(&kw.get().namespace, &kw.get().name),
                    Value::Symbol(s) => qualified(&s.get().namespace, &s.get().name),
                    Value::Long(n) => n.to_string(),
                    other => {
                        return Err(format!(
                            "cannot use a {} as a nu record key",
                            other.type_name()
                        ));
                    }
                };
                record.push(key, clj_to_nu(val)?);
            }
            NuValue::record(record, span)
        }
        other => {
            return Err(format!(
                "cannot convert {} to a nu value",
                other.type_name()
            ));
        }
    })
}
