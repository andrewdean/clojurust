//! A minimal `java.util.HashMap` emulation for portable `.cljc` libraries
//! whose `:clj` branches reach for a mutable map (malli's fast-registry
//! does `(doto (HashMap. ...) (.putAll m))` + `.get`).
//!
//! Constructed by the `HashMap.` / `java.util.HashMap.` builtins; methods
//! dispatch through `cljrs-interp`'s `dispatch_method` NativeObject arm.

use std::collections::HashMap;
use std::sync::Mutex;

use cljrs_value::{NativeObject, NativeObjectBox, Value, ValueResult};

#[derive(Debug)]
pub struct JavaHashMap {
    pub inner: Mutex<HashMap<Value, Value>>,
}

impl NativeObject for JavaHashMap {
    fn type_tag(&self) -> &str {
        "java.util.HashMap"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl cljrs_gc::Trace for JavaHashMap {
    fn trace(&self, visitor: &mut cljrs_gc::MarkVisitor) {
        for (k, v) in self.inner.lock().unwrap().iter() {
            k.trace(visitor);
            v.trace(visitor);
        }
    }
}

/// `(HashMap.)` / `(HashMap. capacity)` / `(HashMap. capacity load-factor)` —
/// sizing arguments are accepted and ignored.
pub fn builtin_hashmap_new(_args: &[Value]) -> ValueResult<Value> {
    Ok(Value::NativeObject(cljrs_value::gc_native_object(
        JavaHashMap {
            inner: Mutex::new(HashMap::new()),
        },
    )))
}

/// Method dispatch for JavaHashMap: the java.util.Map subset scripts use.
pub fn dispatch(map: &JavaHashMap, method: &str, args: &[Value]) -> Option<ValueResult<Value>> {
    let mut inner = map.inner.lock().unwrap();
    Some(match method {
        "get" => Ok(args
            .first()
            .and_then(|k| inner.get(k).cloned())
            .unwrap_or(Value::Nil)),
        "put" => {
            let (Some(k), Some(v)) = (args.first(), args.get(1)) else {
                return Some(Err(cljrs_value::ValueError::Other(
                    ".put requires key and value".into(),
                )));
            };
            Ok(inner.insert(k.clone(), v.clone()).unwrap_or(Value::Nil))
        }
        "putAll" => {
            match args.first() {
                Some(Value::Map(m)) => {
                    m.for_each(|k, v| {
                        inner.insert(k.clone(), v.clone());
                    });
                }
                Some(Value::NativeObject(obj)) => {
                    if let Some(other) = obj.get().downcast_ref::<JavaHashMap>() {
                        for (k, v) in other.inner.lock().unwrap().iter() {
                            inner.insert(k.clone(), v.clone());
                        }
                    }
                }
                _ => {
                    return Some(Err(cljrs_value::ValueError::Other(
                        ".putAll requires a map".into(),
                    )));
                }
            }
            Ok(Value::Nil)
        }
        "containsKey" => Ok(Value::Bool(
            args.first().is_some_and(|k| inner.contains_key(k)),
        )),
        "remove" => Ok(args
            .first()
            .and_then(|k| inner.remove(k))
            .unwrap_or(Value::Nil)),
        "size" => Ok(Value::Long(inner.len() as i64)),
        "isEmpty" => Ok(Value::Bool(inner.is_empty())),
        "clear" => {
            inner.clear();
            Ok(Value::Nil)
        }
        _ => return None,
    })
}

// ── java.util.Iterator over a snapshot ──────────────────────────────────────

#[derive(Debug)]
pub struct JavaIterator {
    items: Mutex<std::collections::VecDeque<Value>>,
}

impl NativeObject for JavaIterator {
    fn type_tag(&self) -> &str {
        "java.util.Iterator"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl cljrs_gc::Trace for JavaIterator {
    fn trace(&self, visitor: &mut cljrs_gc::MarkVisitor) {
        for v in self.items.lock().unwrap().iter() {
            v.trace(visitor);
        }
    }
}

/// `(.iterator coll)` — snapshot the collection (maps iterate as [k v]
/// entries, like Java entry sets reached via seq).
pub fn iterator_of(target: &Value) -> ValueResult<Value> {
    let items = crate::builtins::value_to_seq(target)?;
    Ok(Value::NativeObject(cljrs_value::gc_native_object(
        JavaIterator {
            items: Mutex::new(items.into()),
        },
    )))
}

fn dispatch_iterator(it: &JavaIterator, method: &str, _args: &[Value]) -> Option<ValueResult<Value>> {
    let mut items = it.items.lock().unwrap();
    Some(match method {
        "hasNext" => Ok(Value::Bool(!items.is_empty())),
        "next" => match items.pop_front() {
            Some(v) => Ok(v),
            None => Err(cljrs_value::ValueError::Other(
                "NoSuchElementException: iterator exhausted".into(),
            )),
        },
        _ => return None,
    })
}

/// Route a method call on any java-shim NativeObject; None = not ours.
pub fn dispatch_any(
    obj: &NativeObjectBox,
    method: &str,
    args: &[Value],
) -> Option<ValueResult<Value>> {
    if let Some(m) = obj.downcast_ref::<JavaHashMap>() {
        return dispatch(m, method, args);
    }
    if let Some(it) = obj.downcast_ref::<JavaIterator>() {
        return dispatch_iterator(it, method, args);
    }
    if let Some(dq) = obj.downcast_ref::<JavaArrayDeque>() {
        return dispatch_deque(dq, method, args);
    }
    if obj.downcast_ref::<JavaDateFormatter>().is_some() {
        return dispatch_date_formatter(method, args);
    }
    None
}

// ── java.util.ArrayDeque (used as a stack by malli's regex drivers) ─────────

#[derive(Debug)]
pub struct JavaArrayDeque {
    items: Mutex<std::collections::VecDeque<Value>>,
}

impl NativeObject for JavaArrayDeque {
    fn type_tag(&self) -> &str {
        "java.util.ArrayDeque"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl cljrs_gc::Trace for JavaArrayDeque {
    fn trace(&self, visitor: &mut cljrs_gc::MarkVisitor) {
        for v in self.items.lock().unwrap().iter() {
            v.trace(visitor);
        }
    }
}

pub fn builtin_array_deque_new(_args: &[Value]) -> ValueResult<Value> {
    Ok(Value::NativeObject(cljrs_value::gc_native_object(
        JavaArrayDeque {
            items: Mutex::new(std::collections::VecDeque::new()),
        },
    )))
}

fn dispatch_deque(dq: &JavaArrayDeque, method: &str, args: &[Value]) -> Option<ValueResult<Value>> {
    let mut items = dq.items.lock().unwrap();
    Some(match method {
        // Deque-as-stack: push/pop/peek work on the head.
        "push" | "addFirst" => {
            items.push_front(args.first().cloned().unwrap_or(Value::Nil));
            Ok(Value::Nil)
        }
        "add" | "addLast" | "offer" => {
            items.push_back(args.first().cloned().unwrap_or(Value::Nil));
            Ok(Value::Bool(true))
        }
        "pop" | "removeFirst" => match items.pop_front() {
            Some(v) => Ok(v),
            None => Err(cljrs_value::ValueError::Other(
                "NoSuchElementException: deque is empty".into(),
            )),
        },
        "poll" | "pollFirst" => Ok(items.pop_front().unwrap_or(Value::Nil)),
        "peek" | "peekFirst" => Ok(items.front().cloned().unwrap_or(Value::Nil)),
        "isEmpty" => Ok(Value::Bool(items.is_empty())),
        "size" => Ok(Value::Long(items.len() as i64)),
        "clear" => {
            items.clear();
            Ok(Value::Nil)
        }
        _ => return None,
    })
}

// ── java.time.format.DateTimeFormatter (enough for malli.transform) ─────────
//
// Builder-chain methods return a fresh stateless formatter; parse/format
// speak RFC3339 through the native Instant machinery. Pattern arguments are
// accepted and ignored — cljrsh instants are RFC3339-only.

#[derive(Debug)]
pub struct JavaDateFormatter;

impl NativeObject for JavaDateFormatter {
    fn type_tag(&self) -> &str {
        "java.time.format.DateTimeFormatter"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl cljrs_gc::Trace for JavaDateFormatter {
    fn trace(&self, _visitor: &mut cljrs_gc::MarkVisitor) {}
}

pub fn builtin_date_formatter_new(_args: &[Value]) -> ValueResult<Value> {
    Ok(Value::NativeObject(cljrs_value::gc_native_object(
        JavaDateFormatter,
    )))
}

fn dispatch_date_formatter(method: &str, args: &[Value]) -> Option<ValueResult<Value>> {
    Some(match method {
        // Builder / configuration chain — stateless, return a formatter.
        "appendPattern" | "optionalStart" | "optionalEnd" | "appendFraction"
        | "appendOffset" | "parseDefaulting" | "toFormatter" | "withZone"
        | "appendValue" | "appendLiteral" | "parseLenient" | "parseStrict" => {
            builtin_date_formatter_new(&[])
        }
        "parse" => match args.first() {
            Some(Value::Str(s)) => {
                match cljrs_types::instant::parse_rfc3339_millis(s.get().as_str()) {
                    Ok(ms) => Ok(Value::Instant(ms)),
                    Err(e) => Err(cljrs_value::ValueError::Other(format!(
                        "DateTimeFormatter.parse: {e}"
                    ))),
                }
            }
            _ => Err(cljrs_value::ValueError::Other(
                ".parse expects a string".into(),
            )),
        },
        "format" => match args.first() {
            Some(Value::Instant(ms)) => Ok(Value::string(
                cljrs_types::instant::format_rfc3339_millis(*ms),
            )),
            Some(Value::Long(ms)) => Ok(Value::string(
                cljrs_types::instant::format_rfc3339_millis(*ms),
            )),
            _ => Err(cljrs_value::ValueError::Other(
                ".format expects an instant".into(),
            )),
        },
        _ => return None,
    })
}

/// `(Instant/ofEpochMilli ms)` → native instant.
pub fn builtin_instant_of_epoch_milli(args: &[Value]) -> ValueResult<Value> {
    match &args[0] {
        Value::Long(ms) => Ok(Value::Instant(*ms)),
        other => Err(cljrs_value::ValueError::Other(format!(
            "Instant/ofEpochMilli expects an integer, got {}",
            other.type_name()
        ))),
    }
}

/// `(MapEntry. k v)` — clojure.lang.MapEntry constructor.
pub fn builtin_map_entry_new(args: &[Value]) -> ValueResult<Value> {
    Ok(Value::map_entry(args[0].clone(), args[1].clone()))
}
