//! Minimal transit+json codec — the subset babashka pods use.
//!
//! Transit-JSON is JSON with string-encoding conventions: `"~:kw"` keywords,
//! `"~$sym"` symbols, `"~i123"` out-of-range ints, `"~t..."` instants,
//! `"~u..."` uuids, `"~~"` tilde-escape, `["^ ", k1, v1, ...]` maps,
//! `["~#tag", value]` tagged values, and a **read cache**: cacheable strings
//! (keywords/symbols/tags longer than 3 chars, and map-key strings) are
//! remembered in read order and later referenced as `"^0"`, `"^1"`, ....
//!
//! The encoder never emits cache codes (always legal transit); the decoder
//! implements the cache exactly, since pods' writers do use it.

use cljrs_gc::GcPtr;
use cljrs_value::value::{MapValue, SetValue};
use cljrs_value::{Keyword, PersistentHashSet, PersistentList, PersistentVector, Symbol, Value};
use serde_json::Value as Json;

// ── Decode ────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct ReadCache {
    entries: Vec<CacheEntry>,
}

#[derive(Clone)]
enum CacheEntry {
    /// A decoded scalar (keyword/symbol) referenced by later cache codes.
    Value(Value),
    /// A raw string (map key or tag) cached before interpretation.
    Raw(String),
}

fn cache_code_index(s: &str) -> Option<usize> {
    let rest = s.strip_prefix('^')?;
    if rest == " " {
        return None; // "^ " is the map marker, not a cache code
    }
    let bytes = rest.as_bytes();
    match bytes.len() {
        1 => Some((bytes[0] as usize).checked_sub(48)?),
        2 => {
            let hi = (bytes[0] as usize).checked_sub(48)?;
            let lo = (bytes[1] as usize).checked_sub(48)?;
            Some(hi * 44 + lo)
        }
        _ => None,
    }
}

fn cacheable(s: &str) -> bool {
    s.len() > 3 && (s.starts_with("~:") || s.starts_with("~$") || s.starts_with("~#"))
}

/// Decode a transit-JSON document into a Clojure value.
pub fn decode(json: &Json) -> Result<Value, String> {
    let mut cache = ReadCache::default();
    decode_inner(json, &mut cache, false)
}

fn decode_inner(json: &Json, cache: &mut ReadCache, as_map_key: bool) -> Result<Value, String> {
    Ok(match json {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Long(i)
            } else {
                Value::Double(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        Json::String(s) => decode_string(s, cache, as_map_key)?,
        Json::Array(items) => {
            if let Some(Json::String(head)) = items.first() {
                // Map form: ["^ ", k1, v1, ...]
                if head == "^ " {
                    let mut m = MapValue::empty();
                    let entries = &items[1..];
                    if entries.len() % 2 != 0 {
                        return Err("transit map with odd entry count".to_string());
                    }
                    for pair in entries.chunks(2) {
                        let k = decode_inner(&pair[0], cache, true)?;
                        let v = decode_inner(&pair[1], cache, false)?;
                        m = m.assoc(k, v);
                    }
                    return Ok(Value::Map(m));
                }
                // Tagged form: ["~#tag", value] (tag may arrive via cache).
                let tag = resolve_tag(head, cache);
                if let Some(tag) = tag
                    && items.len() == 2
                {
                    return decode_tagged(&tag, &items[1], cache);
                }
            }
            let vals: Vec<Value> = items
                .iter()
                .map(|i| decode_inner(i, cache, false))
                .collect::<Result<_, _>>()?;
            Value::Vector(GcPtr::new(PersistentVector::from_iter(vals)))
        }
        Json::Object(entries) => {
            // Verbose-mode map (or single-entry tagged value).
            if entries.len() == 1 {
                let (k, v) = entries.iter().next().unwrap();
                if let Some(tag) = k.strip_prefix("~#") {
                    return decode_tagged(tag, v, cache);
                }
            }
            let mut m = MapValue::empty();
            for (k, v) in entries {
                let key = decode_string(k, cache, true)?;
                m = m.assoc(key, decode_inner(v, cache, false)?);
            }
            Value::Map(m)
        }
    })
}

/// A tag string from an array head: either a literal `~#tag` (cached as raw)
/// or a cache code resolving to one.
fn resolve_tag(head: &str, cache: &mut ReadCache) -> Option<String> {
    if let Some(tag) = head.strip_prefix("~#") {
        if cacheable(head) {
            cache.entries.push(CacheEntry::Raw(head.to_string()));
        }
        return Some(tag.to_string());
    }
    if let Some(idx) = cache_code_index(head)
        && let Some(CacheEntry::Raw(raw)) = cache.entries.get(idx).cloned()
    {
        return raw.strip_prefix("~#").map(str::to_string);
    }
    None
}

fn decode_tagged(tag: &str, value: &Json, cache: &mut ReadCache) -> Result<Value, String> {
    match tag {
        "set" => {
            let Json::Array(items) = value else {
                return Err("transit set body must be an array".to_string());
            };
            let mut s = PersistentHashSet::empty();
            for item in items {
                s = s.conj(decode_inner(item, cache, false)?);
            }
            Ok(Value::Set(SetValue::Hash(GcPtr::new(s))))
        }
        "list" => {
            let Json::Array(items) = value else {
                return Err("transit list body must be an array".to_string());
            };
            let vals: Vec<Value> = items
                .iter()
                .map(|i| decode_inner(i, cache, false))
                .collect::<Result<_, _>>()?;
            Ok(Value::List(GcPtr::new(PersistentList::from_iter(vals))))
        }
        "'" => decode_inner(value, cache, false),
        "cmap" => {
            let Json::Array(items) = value else {
                return Err("transit cmap body must be an array".to_string());
            };
            if items.len() % 2 != 0 {
                return Err("transit cmap with odd entry count".to_string());
            }
            let mut m = MapValue::empty();
            for pair in items.chunks(2) {
                m = m.assoc(
                    decode_inner(&pair[0], cache, false)?,
                    decode_inner(&pair[1], cache, false)?,
                );
            }
            Ok(Value::Map(m))
        }
        other => Err(format!("unsupported transit tag ~#{other}")),
    }
}

fn decode_string(s: &str, cache: &mut ReadCache, as_map_key: bool) -> Result<Value, String> {
    if let Some(idx) = cache_code_index(s) {
        return match cache.entries.get(idx) {
            Some(CacheEntry::Value(v)) => Ok(v.clone()),
            Some(CacheEntry::Raw(raw)) => {
                let raw = raw.clone();
                decode_uncached(&raw, cache, as_map_key, false)
            }
            None => Err(format!("transit cache miss for {s:?}")),
        };
    }
    decode_uncached(s, cache, as_map_key, true)
}

fn decode_uncached(
    s: &str,
    cache: &mut ReadCache,
    as_map_key: bool,
    may_cache: bool,
) -> Result<Value, String> {
    let value = if let Some(kw) = s.strip_prefix("~:") {
        Value::keyword(Keyword::parse(kw))
    } else if let Some(sym) = s.strip_prefix("~$") {
        Value::symbol(Symbol::parse(sym))
    } else if let Some(i) = s.strip_prefix("~i") {
        Value::Long(i.parse::<i64>().map_err(|e| format!("bad ~i int: {e}"))?)
    } else if let Some(d) = s.strip_prefix("~d") {
        Value::Double(d.parse::<f64>().map_err(|e| format!("bad ~d double: {e}"))?)
    } else if let Some(t) = s.strip_prefix("~t") {
        Value::Instant(cljrs_types::instant::parse_rfc3339_millis(t)?)
    } else if let Some(u) = s.strip_prefix("~u") {
        Value::Uuid(
            uuid::Uuid::parse_str(u)
                .map_err(|e| format!("bad ~u uuid: {e}"))?
                .as_u128(),
        )
    } else if let Some(rest) = s.strip_prefix("~~") {
        Value::string(format!("~{rest}"))
    } else if let Some(rest) = s.strip_prefix("~^") {
        Value::string(format!("^{rest}"))
    } else if s.starts_with("~#") {
        return Err(format!("stray transit tag {s:?}"));
    } else if s.starts_with('~') {
        return Err(format!("unsupported transit encoding {s:?}"));
    } else {
        Value::string(s.to_string())
    };

    // Read-cache bookkeeping mirrors the writer: keywords/symbols > 3 chars
    // always cache; plain strings cache only in map-key position.
    if may_cache {
        let is_kw_or_sym = s.starts_with("~:") || s.starts_with("~$");
        if (is_kw_or_sym && s.len() > 3) || (as_map_key && !s.starts_with('~') && s.len() > 3) {
            cache.entries.push(CacheEntry::Value(value.clone()));
        }
    }
    Ok(value)
}

// ── Encode ────────────────────────────────────────────────────────────────────

/// Encode a Clojure value as transit-JSON (no cache codes — always legal).
pub fn encode(v: &Value) -> Result<Json, String> {
    Ok(match v {
        Value::Nil => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::Long(n) => {
            // JSON numbers are only safe to 2^53; beyond that use ~i.
            if n.abs() <= (1i64 << 53) {
                Json::from(*n)
            } else {
                Json::String(format!("~i{n}"))
            }
        }
        Value::Double(d) => {
            if d.is_finite() {
                Json::from(*d)
            } else {
                return Err(format!("cannot transit-encode non-finite double {d}"));
            }
        }
        Value::Str(s) => encode_string(s.get()),
        Value::Char(c) => encode_string(&c.to_string()),
        Value::Keyword(k) => Json::String(format!("~:{}", qualified(&k.get().namespace, &k.get().name))),
        Value::Symbol(s) => Json::String(format!("~${}", qualified(&s.get().namespace, &s.get().name))),
        Value::Instant(ms) => Json::String(format!(
            "~t{}",
            cljrs_types::instant::format_rfc3339_millis(*ms)
        )),
        Value::Uuid(u) => Json::String(format!("~u{}", uuid::Uuid::from_u128(*u))),
        Value::Vector(items) => Json::Array(
            items
                .get()
                .iter()
                .map(encode)
                .collect::<Result<_, _>>()?,
        ),
        Value::List(items) => Json::Array(vec![
            Json::String("~#list".to_string()),
            Json::Array(items.get().iter().map(encode).collect::<Result<_, _>>()?),
        ]),
        Value::Set(set) => {
            let items: Vec<Json> = match set {
                SetValue::Hash(s) => s.get().iter().map(encode).collect::<Result<_, _>>()?,
                SetValue::Sorted(s) => s.get().iter().map(encode).collect::<Result<_, _>>()?,
            };
            Json::Array(vec![Json::String("~#set".to_string()), Json::Array(items)])
        }
        Value::Map(m) => {
            let mut out = vec![Json::String("^ ".to_string())];
            for (k, val) in m.iter() {
                out.push(encode(k)?);
                out.push(encode(val)?);
            }
            Json::Array(out)
        }
        other => {
            return Err(format!(
                "cannot transit-encode a {}",
                other.type_name()
            ));
        }
    })
}

fn encode_string(s: &str) -> Json {
    if s.starts_with('~') || s.starts_with('^') {
        Json::String(format!("~{s}"))
    } else {
        Json::String(s.to_string())
    }
}

fn qualified(ns: &Option<std::sync::Arc<str>>, name: &str) -> String {
    match ns {
        Some(ns) => format!("{ns}/{name}"),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dec(s: &str) -> Value {
        decode(&serde_json::from_str(s).unwrap()).unwrap()
    }

    #[test]
    fn scalars_and_keywords() {
        assert_eq!(dec("null"), Value::Nil);
        assert_eq!(dec("42"), Value::Long(42));
        assert_eq!(dec("\"plain\""), Value::string("plain".to_string()));
        assert_eq!(dec("\"~:name\""), Value::keyword(Keyword::simple("name")));
        assert_eq!(dec("\"~i9007199254740993\""), Value::Long(9007199254740993));
        assert_eq!(dec("\"~~tilde\""), Value::string("~tilde".to_string()));
    }

    #[test]
    fn maps_with_cache_codes() {
        // Two rows sharing cached keys — the go-sqlite3 query shape.
        let v = dec(r#"[["^ ","~:name","alice","~:age",42],["^ ","^0","bob","^1",7]]"#);
        let Value::Vector(rows) = &v else { panic!() };
        assert_eq!(rows.get().count(), 2);
        let Value::Map(second) = rows.get().nth(1).unwrap().clone() else {
            panic!()
        };
        assert_eq!(
            second.get(&Value::keyword(Keyword::simple("age"))),
            Some(Value::Long(7))
        );
    }

    #[test]
    fn tagged_set_and_quote() {
        let v = dec(r#"["~#set",[1,2]]"#);
        assert!(matches!(v, Value::Set(_)));
        assert_eq!(dec(r#"["~#'",5]"#), Value::Long(5));
    }

    #[test]
    fn encode_roundtrip() {
        let mut m = MapValue::empty();
        m = m.assoc(
            Value::keyword(Keyword::simple("id")),
            Value::Long(1),
        );
        m = m.assoc(
            Value::keyword(Keyword::simple("when")),
            Value::Instant(1000),
        );
        let v = Value::Vector(GcPtr::new(PersistentVector::from_iter([
            Value::Map(m),
            Value::string("~needs-escape".to_string()),
        ])));
        let json = encode(&v).unwrap();
        let back = decode(&json).unwrap();
        assert_eq!(back, v);
    }
}
