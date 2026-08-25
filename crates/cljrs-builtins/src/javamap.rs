//! A minimal `java.util.HashMap` emulation for portable `.cljc` libraries
//! whose `:clj` branches reach for a mutable map (malli's fast-registry
//! does `(doto (HashMap. ...) (.putAll m))` + `.get`).
//!
//! Constructed by the `HashMap.` / `java.util.HashMap.` builtins; methods
//! dispatch through `cljrs-interp`'s `dispatch_method` NativeObject arm.

// Insertion-ordered maps/sets: java.util collections iterate
// deterministically for a given insertion sequence, and vendored code
// (datalevin's aggregation sinks in particular) relies on two identical
// runs producing identical iteration order. std's randomized hashing
// breaks that, so the shims are backed by indexmap.
use indexmap::{IndexMap, IndexSet};
use std::sync::Mutex;

use cljrs_value::error::ValueError;
use cljrs_value::{NativeObject, NativeObjectBox, Value, ValueResult};

#[derive(Debug)]
pub struct JavaHashMap {
    pub inner: Mutex<IndexMap<Value, Value>>,
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
            inner: Mutex::new(IndexMap::new()),
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
        "putIfAbsent" => {
            let (Some(k), Some(v)) = (args.first(), args.get(1)) else {
                return Some(Err(cljrs_value::ValueError::Other(
                    ".putIfAbsent requires key and value".into(),
                )));
            };
            match inner.get(k) {
                Some(existing) => Ok(existing.clone()),
                None => {
                    inner.insert(k.clone(), v.clone());
                    Ok(Value::Nil)
                }
            }
        }
        "remove" => Ok(args
            .first()
            .and_then(|k| inner.shift_remove(k))
            .unwrap_or(Value::Nil)),
        "size" => Ok(Value::Long(inner.len() as i64)),
        "isEmpty" => Ok(Value::Bool(inner.is_empty())),
        "clear" => {
            inner.clear();
            Ok(Value::Nil)
        }
        // Snapshot views (Java returns live views; scripts only read them).
        "values" => Ok(Value::Vector(cljrs_gc::GcPtr::new(
            cljrs_value::PersistentVector::from_iter(inner.values().cloned()),
        ))),
        "keySet" => Ok(Value::Vector(cljrs_gc::GcPtr::new(
            cljrs_value::PersistentVector::from_iter(inner.keys().cloned()),
        ))),
        "entrySet" => Ok(Value::Vector(cljrs_gc::GcPtr::new(
            cljrs_value::PersistentVector::from_iter(
                inner
                    .iter()
                    .map(|(k, v)| Value::map_entry(k.clone(), v.clone())),
            ),
        ))),
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

fn dispatch_iterator(
    it: &JavaIterator,
    method: &str,
    _args: &[Value],
) -> Option<ValueResult<Value>> {
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
    if let Some(l) = obj.downcast_ref::<JavaArrayList>() {
        return dispatch_array_list(l, method, args);
    }
    if let Some(f) = obj.downcast_ref::<JavaCompletableFuture>() {
        return dispatch_completable_future(f, method, args);
    }
    if let Some(s) = obj.downcast_ref::<JavaHashSet>() {
        return dispatch_hash_set(s, method, args);
    }
    if let Some(dq) = obj.downcast_ref::<JavaArrayDeque>() {
        return dispatch_deque(dq, method, args);
    }
    if obj.downcast_ref::<JavaDateFormatter>().is_some() {
        return dispatch_date_formatter(method, args);
    }
    if let Some(sb) = obj.downcast_ref::<JavaStringBuilder>() {
        return dispatch_string_builder(sb, method, args);
    }
    None
}

// ── java.util.ArrayList / FastList and java.util.HashSet ───────────────────
// Mutable list and set shims so vendored JVM-flavored code (datalevin's
// query engine in particular) runs its FastList/HashSet idioms unchanged.

#[derive(Debug)]
pub struct JavaArrayList {
    pub items: Mutex<Vec<Value>>,
}

impl NativeObject for JavaArrayList {
    fn type_tag(&self) -> &str {
        "java.util.ArrayList"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl cljrs_gc::Trace for JavaArrayList {
    fn trace(&self, visitor: &mut cljrs_gc::MarkVisitor) {
        for v in self.items.lock().unwrap().iter() {
            v.trace(visitor);
        }
    }
}

/// `(ArrayList.)` / `(FastList.)` — an int argument is a capacity hint
/// (ignored); a collection argument copies its elements.
pub fn builtin_array_list_new(args: &[Value]) -> ValueResult<Value> {
    let items = match args.first().map(Value::unwrap_meta) {
        None | Some(Value::Nil) | Some(Value::Long(_)) => Vec::new(),
        Some(other) => crate::builtins::value_to_seq(other)?,
    };
    Ok(Value::NativeObject(cljrs_value::gc_native_object(
        JavaArrayList {
            items: Mutex::new(items),
        },
    )))
}

fn index_arg(args: &[Value], what: &str) -> ValueResult<usize> {
    match args.first().map(Value::unwrap_meta) {
        Some(Value::Long(n)) if *n >= 0 => Ok(*n as usize),
        _ => Err(cljrs_value::ValueError::Other(format!(
            "{what} requires a non-negative index"
        ))),
    }
}

fn dispatch_array_list(
    l: &JavaArrayList,
    method: &str,
    args: &[Value],
) -> Option<ValueResult<Value>> {
    let mut items = l.items.lock().unwrap();
    Some(match method {
        "add" => match (args.first(), args.get(1)) {
            (Some(v), None) => {
                items.push(v.clone());
                Ok(Value::Bool(true))
            }
            (Some(Value::Long(i)), Some(v)) if *i >= 0 && (*i as usize) <= items.len() => {
                items.insert(*i as usize, v.clone());
                Ok(Value::Nil)
            }
            _ => Err(cljrs_value::ValueError::Other(
                ".add requires (value) or (index value)".into(),
            )),
        },
        "addAll" => {
            let vals = match args.first() {
                Some(v) => match crate::builtins::value_to_seq(v.unwrap_meta()) {
                    Ok(vals) => vals,
                    Err(e) => return Some(Err(e)),
                },
                None => {
                    return Some(Err(cljrs_value::ValueError::Other(
                        ".addAll requires a collection".into(),
                    )));
                }
            };
            let changed = !vals.is_empty();
            items.extend(vals);
            Ok(Value::Bool(changed))
        }
        "get" => {
            let idx = match index_arg(args, ".get") {
                Ok(i) => i,
                Err(e) => return Some(Err(e)),
            };
            match items.get(idx) {
                Some(v) => Ok(v.clone()),
                None => Err(cljrs_value::ValueError::Other(format!(
                    "IndexOutOfBoundsException: {idx} of {}",
                    items.len()
                ))),
            }
        }
        "set" => {
            let idx = match index_arg(args, ".set") {
                Ok(i) => i,
                Err(e) => return Some(Err(e)),
            };
            let Some(v) = args.get(1) else {
                return Some(Err(cljrs_value::ValueError::Other(
                    ".set requires index and value".into(),
                )));
            };
            match items.get_mut(idx) {
                Some(slot) => {
                    let old = slot.clone();
                    *slot = v.clone();
                    Ok(old)
                }
                None => Err(cljrs_value::ValueError::Other(format!(
                    "IndexOutOfBoundsException: {idx} of {}",
                    items.len()
                ))),
            }
        }
        // Java overload semantics: an integer argument removes by index
        // (returning the element), anything else removes by value.
        "remove" => match args.first().map(Value::unwrap_meta) {
            Some(Value::Long(i)) if *i >= 0 && (*i as usize) < items.len() => {
                Ok(items.remove(*i as usize))
            }
            Some(v) => {
                if let Some(pos) = items.iter().position(|x| x == v) {
                    items.remove(pos);
                    Ok(Value::Bool(true))
                } else {
                    Ok(Value::Bool(false))
                }
            }
            None => Err(cljrs_value::ValueError::Other(
                ".remove requires an argument".into(),
            )),
        },
        "contains" => Ok(Value::Bool(args.first().is_some_and(|v| items.contains(v)))),
        "indexOf" => Ok(Value::Long(
            args.first()
                .and_then(|v| items.iter().position(|x| x == v))
                .map_or(-1, |i| i as i64),
        )),
        "size" => Ok(Value::Long(items.len() as i64)),
        "isEmpty" => Ok(Value::Bool(items.is_empty())),
        "clear" => {
            items.clear();
            Ok(Value::Nil)
        }
        "toArray" => Ok(Value::ObjectArray(cljrs_gc::GcPtr::new(
            cljrs_value::ObjectArray::new(items.clone()),
        ))),
        _ => return None,
    })
}

#[derive(Debug)]
pub struct JavaHashSet {
    pub items: Mutex<IndexSet<Value>>,
}

impl NativeObject for JavaHashSet {
    fn type_tag(&self) -> &str {
        "java.util.HashSet"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl cljrs_gc::Trace for JavaHashSet {
    fn trace(&self, visitor: &mut cljrs_gc::MarkVisitor) {
        for v in self.items.lock().unwrap().iter() {
            v.trace(visitor);
        }
    }
}

/// `(HashSet.)` — an int argument is a capacity hint (ignored); a
/// collection argument copies its elements.
// Value contains interior-mutable cells (atoms); as on the JVM, mutating
// an element while it sits in a HashSet is undefined behavior for the set.
#[allow(clippy::mutable_key_type)]
pub fn builtin_hash_set_new(args: &[Value]) -> ValueResult<Value> {
    let items: IndexSet<Value> = match args.first().map(Value::unwrap_meta) {
        None | Some(Value::Nil) | Some(Value::Long(_)) => Default::default(),
        Some(other) => crate::builtins::value_to_seq(other)?.into_iter().collect(),
    };
    Ok(Value::NativeObject(cljrs_value::gc_native_object(
        JavaHashSet {
            items: Mutex::new(items),
        },
    )))
}

fn dispatch_hash_set(s: &JavaHashSet, method: &str, args: &[Value]) -> Option<ValueResult<Value>> {
    let mut items = s.items.lock().unwrap();
    Some(match method {
        "add" => match args.first() {
            Some(v) => Ok(Value::Bool(items.insert(v.clone()))),
            None => Err(cljrs_value::ValueError::Other(
                ".add requires an argument".into(),
            )),
        },
        "addAll" => {
            let vals = match args.first() {
                Some(v) => match crate::builtins::value_to_seq(v.unwrap_meta()) {
                    Ok(vals) => vals,
                    Err(e) => return Some(Err(e)),
                },
                None => {
                    return Some(Err(cljrs_value::ValueError::Other(
                        ".addAll requires a collection".into(),
                    )));
                }
            };
            let mut changed = false;
            for v in vals {
                changed |= items.insert(v);
            }
            Ok(Value::Bool(changed))
        }
        "contains" => Ok(Value::Bool(args.first().is_some_and(|v| items.contains(v)))),
        "remove" => Ok(Value::Bool(
            args.first().is_some_and(|v| items.shift_remove(v)),
        )),
        "size" => Ok(Value::Long(items.len() as i64)),
        "isEmpty" => Ok(Value::Bool(items.is_empty())),
        "clear" => {
            items.clear();
            Ok(Value::Nil)
        }
        "toArray" => Ok(Value::ObjectArray(cljrs_gc::GcPtr::new(
            cljrs_value::ObjectArray::new(items.iter().cloned().collect()),
        ))),
        _ => return None,
    })
}

/// Snapshot the elements of a java-shim mutable collection; None = not
/// one. Maps iterate as [k v] entries. Lets seq/count/nth/vec treat the
/// shims as ordinary collections.
pub fn native_coll_items(v: &Value) -> Option<Vec<Value>> {
    let Value::NativeObject(obj) = v else {
        return None;
    };
    let o = obj.get();
    if let Some(l) = o.downcast_ref::<JavaArrayList>() {
        return Some(l.items.lock().unwrap().clone());
    }
    if let Some(s) = o.downcast_ref::<JavaHashSet>() {
        return Some(s.items.lock().unwrap().iter().cloned().collect());
    }
    if let Some(m) = o.downcast_ref::<JavaHashMap>() {
        return Some(
            m.inner
                .lock()
                .unwrap()
                .iter()
                .map(|(k, v)| Value::map_entry(k.clone(), v.clone()))
                .collect(),
        );
    }
    None
}

/// Element count of a java-shim mutable collection; None = not one.
pub fn native_coll_count(v: &Value) -> Option<usize> {
    let Value::NativeObject(obj) = v else {
        return None;
    };
    let o = obj.get();
    if let Some(l) = o.downcast_ref::<JavaArrayList>() {
        return Some(l.items.lock().unwrap().len());
    }
    if let Some(s) = o.downcast_ref::<JavaHashSet>() {
        return Some(s.items.lock().unwrap().len());
    }
    if let Some(m) = o.downcast_ref::<JavaHashMap>() {
        return Some(m.inner.lock().unwrap().len());
    }
    None
}

// ── java.util.concurrent.CompletableFuture — a settable box ─────────────────
// Single-threaded runtime: complete stores the value, get returns it (or
// raises when nothing has been delivered — the JVM would block forever).

#[derive(Debug)]
pub struct JavaCompletableFuture {
    pub slot: Mutex<Option<Value>>,
}

impl NativeObject for JavaCompletableFuture {
    fn type_tag(&self) -> &str {
        "java.util.concurrent.CompletableFuture"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl cljrs_gc::Trace for JavaCompletableFuture {
    fn trace(&self, visitor: &mut cljrs_gc::MarkVisitor) {
        if let Some(v) = self.slot.lock().unwrap().as_ref() {
            v.trace(visitor);
        }
    }
}

pub fn builtin_completable_future_new(_args: &[Value]) -> ValueResult<Value> {
    Ok(Value::NativeObject(cljrs_value::gc_native_object(
        JavaCompletableFuture {
            slot: Mutex::new(None),
        },
    )))
}

fn dispatch_completable_future(
    f: &JavaCompletableFuture,
    method: &str,
    args: &[Value],
) -> Option<ValueResult<Value>> {
    let mut slot = f.slot.lock().unwrap();
    Some(match method {
        "complete" => {
            let was_empty = slot.is_none();
            if was_empty {
                *slot = Some(args.first().cloned().unwrap_or(Value::Nil));
            }
            Ok(Value::Bool(was_empty))
        }
        "get" | "join" => match slot.as_ref() {
            Some(v) => Ok(v.clone()),
            None => Err(cljrs_value::ValueError::Other(
                "CompletableFuture.get: nothing delivered (single-threaded runtime would deadlock)"
                    .into(),
            )),
        },
        "isDone" => Ok(Value::Bool(slot.is_some())),
        _ => return None,
    })
}

// ── java.lang.Object — the unique-sentinel idiom ────────────────────────────

#[derive(Debug)]
pub struct JavaObject;

impl NativeObject for JavaObject {
    fn type_tag(&self) -> &str {
        "java.lang.Object"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl cljrs_gc::Trace for JavaObject {
    fn trace(&self, _visitor: &mut cljrs_gc::MarkVisitor) {}
}

/// `(Object.)` — a fresh object equal only to itself.
pub fn builtin_object_new(_args: &[Value]) -> ValueResult<Value> {
    Ok(Value::NativeObject(cljrs_value::gc_native_object(
        JavaObject,
    )))
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
        "appendPattern" | "optionalStart" | "optionalEnd" | "appendFraction" | "appendOffset"
        | "parseDefaulting" | "toFormatter" | "withZone" | "appendValue" | "appendLiteral"
        | "parseLenient" | "parseStrict" => builtin_date_formatter_new(&[]),
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

// ── java.lang.StringBuilder (honeysql's join, string-building :clj paths) ────

#[derive(Debug)]
pub struct JavaStringBuilder {
    pub buf: Mutex<String>,
}

impl NativeObject for JavaStringBuilder {
    fn type_tag(&self) -> &str {
        "StringBuilder"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl cljrs_gc::Trace for JavaStringBuilder {
    fn trace(&self, _visitor: &mut cljrs_gc::MarkVisitor) {}
}

pub fn builtin_string_builder_new(args: &[Value]) -> ValueResult<Value> {
    let init = match args.first() {
        Some(Value::Str(s)) => s.get().clone(),
        _ => String::new(),
    };
    Ok(Value::NativeObject(cljrs_value::gc_native_object(
        JavaStringBuilder {
            buf: Mutex::new(init),
        },
    )))
}

fn str_content(v: &Value) -> String {
    match v {
        Value::Nil => String::new(),
        Value::Str(s) => s.get().to_string(),
        Value::Char(c) => c.to_string(),
        other => format!("{other}"),
    }
}

fn dispatch_string_builder(
    sb: &JavaStringBuilder,
    method: &str,
    args: &[Value],
) -> Option<ValueResult<Value>> {
    Some(match method {
        "append" => {
            let mut buf = sb.buf.lock().unwrap();
            for a in args {
                buf.push_str(&str_content(a));
            }
            // Chaining callers use doto; the mutation is the point.
            Ok(Value::Nil)
        }
        "toString" => Ok(Value::string(sb.buf.lock().unwrap().clone())),
        "length" => Ok(Value::Long(sb.buf.lock().unwrap().chars().count() as i64)),
        "setLength" => {
            if let Some(Value::Long(n)) = args.first() {
                let mut buf = sb.buf.lock().unwrap();
                let keep: String = buf.chars().take(*n as usize).collect();
                *buf = keep;
            }
            Ok(Value::Nil)
        }
        _ => {
            return Some(Err(ValueError::Other(format!(
                ".{method} not supported on StringBuilder"
            ))));
        }
    })
}

/// `get` semantics for the shims, mirroring the JVM's `RT.get` on
/// `java.util.Map` (key lookup) and set membership for `HashSet`.
/// None = not a shim the caller should fall through on.
pub fn native_coll_get(v: &Value, key: &Value) -> Option<Value> {
    let Value::NativeObject(obj) = v else {
        return None;
    };
    let o = obj.get();
    if let Some(m) = o.downcast_ref::<JavaHashMap>() {
        return Some(
            m.inner
                .lock()
                .unwrap()
                .get(key)
                .cloned()
                .unwrap_or(Value::Nil),
        );
    }
    if let Some(s) = o.downcast_ref::<JavaHashSet>() {
        return Some(if s.items.lock().unwrap().contains(key) {
            key.clone()
        } else {
            Value::Nil
        });
    }
    if let Some(l) = o.downcast_ref::<JavaArrayList>() {
        if let Value::Long(i) = key {
            return Some(
                l.items
                    .lock()
                    .unwrap()
                    .get(*i as usize)
                    .cloned()
                    .unwrap_or(Value::Nil),
            );
        }
        return Some(Value::Nil);
    }
    None
}
