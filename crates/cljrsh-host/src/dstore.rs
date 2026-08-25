//! `cljrs.dstore.native` — the durable datalog store (cljrs-datalog-store)
//! exposed to Clojure. The `cljrs.dstore` source namespace wraps these
//! natives in a datascript-protocol DB so `q` and `pull` run against disk.
//!
//! Value conversion at this boundary is attr-aware: integer values of
//! ref-typed attributes become [`StoreValue::Ref`] so the store maintains
//! its reverse index; everything else maps 1:1 onto codec types.

use std::path::Path;
use std::sync::Arc;

use cljrs_datalog_store::{AttrProps, Bound, Datom, Index, Op, Store, StoreValue};
use cljrs_gc::{GcPtr, MarkVisitor, Trace};
use cljrs_interop::{Registry, wrap_fn_variadic, wrap_fn1, wrap_fn2};
use cljrs_value::value::MapValue;
use cljrs_value::{Keyword, NativeObject, PersistentVector, Value, gc_native_object};

pub const NS: &str = "cljrs.dstore.native";

#[derive(Debug)]
struct StoreHandle {
    store: Arc<Store>,
}

impl Trace for StoreHandle {
    fn trace(&self, _visitor: &mut MarkVisitor) {}
}

impl NativeObject for StoreHandle {
    fn type_tag(&self) -> &str {
        "DatalogStore"
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn kw(name: &str) -> Value {
    Value::keyword(Keyword::simple(name))
}

fn with_store<R>(v: &Value, f: impl FnOnce(&Store) -> Result<R, String>) -> Result<R, String> {
    match v {
        Value::NativeObject(obj) if obj.get().type_tag() == "DatalogStore" => {
            let handle = obj
                .get()
                .downcast_ref::<StoreHandle>()
                .ok_or_else(|| "corrupt DatalogStore handle".to_string())?;
            f(&handle.store)
        }
        other => Err(format!(
            "expected a DatalogStore, got {}",
            other.type_name()
        )),
    }
}

fn str_arg(v: &Value, what: &str) -> Result<String, String> {
    match v {
        Value::Str(s) => Ok(s.get().to_string()),
        other => Err(format!(
            "{what} must be a string, got {}",
            other.type_name()
        )),
    }
}

/// Attribute names cross the boundary as keywords; stored as `ns/name`.
fn attr_arg(v: &Value, what: &str) -> Result<String, String> {
    match v {
        Value::Keyword(k) => Ok(k.get().full_name()),
        Value::Str(s) => Ok(s.get().to_string()),
        other => Err(format!(
            "{what} must be a keyword, got {}",
            other.type_name()
        )),
    }
}

fn eid_arg(v: &Value, what: &str) -> Result<u64, String> {
    match v {
        Value::Long(n) if *n >= 0 => Ok(*n as u64),
        other => Err(format!(
            "{what} must be a non-negative integer, got {}",
            other.type_name()
        )),
    }
}

/// Clojure value → store value, coercing integers to refs for ref attrs.
fn to_store_value(store: &Store, attr: Option<&str>, v: &Value) -> Result<StoreValue, String> {
    let ref_attr = attr
        .and_then(|a| store.attr_props(a))
        .is_some_and(|p| p.ref_type);
    Ok(match v.unwrap_meta() {
        Value::Long(n) if ref_attr && *n >= 0 => StoreValue::Ref(*n as u64),
        Value::Long(n) => StoreValue::Long(*n),
        Value::Bool(b) => StoreValue::Bool(*b),
        Value::Double(d) => StoreValue::Double(*d),
        Value::Str(s) => StoreValue::Str(s.get().to_string()),
        Value::Keyword(k) => StoreValue::Keyword(k.get().full_name()),
        Value::Uuid(u) => StoreValue::Uuid(u.to_be_bytes()),
        Value::Instant(ms) => StoreValue::Instant(*ms),
        seq @ (Value::Vector(_) | Value::List(_) | Value::Cons(_) | Value::LazySeq(_)) => {
            StoreValue::Vec(
                cljrs_value::value::value_to_seq_vec(seq)
                    .iter()
                    .map(|item| to_store_value(store, None, item))
                    .collect::<Result<Vec<_>, _>>()?,
            )
        }
        other => {
            return Err(format!(
                "unsupported datom value type: {}",
                other.type_name()
            ));
        }
    })
}

fn from_store_value(v: &StoreValue) -> Value {
    match v {
        StoreValue::Bool(b) => Value::Bool(*b),
        StoreValue::Long(n) => Value::Long(*n),
        StoreValue::Double(d) => Value::Double(*d),
        StoreValue::Instant(ms) => Value::Instant(*ms),
        StoreValue::Str(s) => Value::string(s.clone()),
        StoreValue::Keyword(s) => Value::keyword(Keyword::parse(s)),
        StoreValue::Uuid(b) => Value::Uuid(u128::from_be_bytes(*b)),
        StoreValue::Ref(e) => Value::Long(*e as i64),
        StoreValue::Bytes(b) => {
            let signed: Vec<i8> = b.iter().map(|x| *x as i8).collect();
            Value::ByteArray(GcPtr::new(std::sync::Mutex::new(signed)))
        }
        StoreValue::Vec(items) => Value::Vector(GcPtr::new(PersistentVector::from_iter(
            items.iter().map(from_store_value),
        ))),
    }
}

/// A datom crosses back as `[e attr-keyword v]`.
fn datom_value(d: &Datom) -> Value {
    Value::Vector(GcPtr::new(PersistentVector::from_iter([
        Value::Long(d.e as i64),
        Value::keyword(Keyword::parse(&d.a)),
        from_store_value(&d.v),
    ])))
}

fn datoms_value(ds: &[Datom]) -> Value {
    Value::Vector(GcPtr::new(PersistentVector::from_iter(
        ds.iter().map(datom_value),
    )))
}

fn opt_eid(v: Option<&Value>) -> Result<Option<u64>, String> {
    match v.map(Value::unwrap_meta) {
        None | Some(Value::Nil) => Ok(None),
        Some(val) => Ok(Some(eid_arg(val, "entity id")?)),
    }
}

fn opt_attr(v: Option<&Value>) -> Result<Option<String>, String> {
    match v.map(Value::unwrap_meta) {
        None | Some(Value::Nil) => Ok(None),
        Some(val) => Ok(Some(attr_arg(val, "attribute")?)),
    }
}

fn props_from_map(v: &Value) -> Result<AttrProps, String> {
    let mut props = AttrProps::default();
    if let Value::Map(m) = v.unwrap_meta() {
        if let Some(Value::Bool(true)) = m.get(&Value::keyword(Keyword::simple("cardinality-many")))
        {
            props.cardinality_many = true;
        }
        if let Some(Value::Bool(true)) = m.get(&Value::keyword(Keyword::simple("ref"))) {
            props.ref_type = true;
        }
        if let Some(Value::Bool(true)) = m.get(&Value::keyword(Keyword::simple("unique-identity")))
        {
            props.unique_identity = true;
        }
        if let Some(Value::Bool(true)) = m.get(&Value::keyword(Keyword::simple("unique-value"))) {
            props.unique_value = true;
        }
    }
    Ok(props)
}

fn index_arg(v: &Value) -> Result<Index, String> {
    match v.unwrap_meta() {
        Value::Keyword(k) => match k.get().full_name().as_str() {
            "eav" => Ok(Index::Eav),
            "ave" => Ok(Index::Ave),
            "vae" => Ok(Index::Vae),
            other => Err(format!("unknown index: :{other}")),
        },
        other => Err(format!(
            "index must be a keyword, got {}",
            other.type_name()
        )),
    }
}

/// (entity, attribute, value, closed?) with nil wildcards as None.
type BoundParts = (Option<u64>, Option<String>, Option<StoreValue>, bool);

/// A bound crosses as `[e a v]` or `[e a v closed?]` with nil wildcards;
/// nil (or an absent arg) is the unbounded bound.
fn bound_parts(store: &Store, index: Index, v: Option<&Value>) -> Result<BoundParts, String> {
    let val = match v.map(Value::unwrap_meta) {
        None | Some(Value::Nil) => return Ok((None, None, None, true)),
        Some(val) => val,
    };
    let Value::Vector(parts) = val else {
        return Err("bound must be a vector [e a v] or [e a v closed?]".into());
    };
    let parts: Vec<Value> = parts.get().iter().cloned().collect();
    let e = opt_eid(parts.first())?;
    let a = opt_attr(parts.get(1))?;
    let sv = match parts.get(2).map(Value::unwrap_meta) {
        None | Some(Value::Nil) => None,
        // The vae index keys refs; a bare integer there is a ref target.
        Some(Value::Long(n)) if index == Index::Vae && *n >= 0 => Some(StoreValue::Ref(*n as u64)),
        Some(val) => Some(to_store_value(store, a.as_deref(), val)?),
    };
    let closed = !matches!(
        parts.get(3).map(Value::unwrap_meta),
        Some(Value::Bool(false))
    );
    Ok((e, a, sv, closed))
}

fn slice_impl(store: &Store, args: &[Value], reverse: bool) -> Result<Value, String> {
    let index = index_arg(&args[1])?;
    let (le, la, lv, lc) = bound_parts(store, index, args.get(2))?;
    let (he, ha, hv, hc) = bound_parts(store, index, args.get(3))?;
    let low = Bound {
        e: le,
        a: la.as_deref(),
        v: lv.as_ref(),
        closed: lc,
    };
    let high = Bound {
        e: he,
        a: ha.as_deref(),
        v: hv.as_ref(),
        closed: hc,
    };
    let limit = match args.get(4).map(Value::unwrap_meta) {
        None | Some(Value::Nil) => None,
        Some(Value::Long(n)) if *n >= 0 => Some(*n as usize),
        Some(other) => {
            return Err(format!(
                "limit must be a non-negative integer, got {}",
                other.type_name()
            ));
        }
    };
    let ds = store
        .slice(index, &low, &high, limit, reverse)
        .map_err(|e| e.to_string())?;
    Ok(datoms_value(&ds))
}

fn search_impl(store: &Store, args: &[Value]) -> Result<Value, String> {
    let e = opt_eid(args.get(1))?;
    let a = opt_attr(args.get(2))?;
    let v_raw = match args.get(3).map(Value::unwrap_meta) {
        None | Some(Value::Nil) => None,
        Some(val) => Some(val.clone()),
    };
    let a_ref = a.as_deref();
    let sv = v_raw
        .as_ref()
        .map(|val| to_store_value(store, a_ref, val))
        .transpose()?;
    let mut datoms = store
        .search(e, a_ref, sv.as_ref())
        .map_err(|err| err.to_string())?;
    // An unbound-attr integer value may match either plain longs or refs:
    // merge the reverse-index hits (the sets are disjoint since an
    // attribute is either ref-typed or not).
    if a_ref.is_none()
        && let Some(StoreValue::Long(n)) = &sv
        && *n >= 0
    {
        let refs = store
            .search(e, None, Some(&StoreValue::Ref(*n as u64)))
            .map_err(|err| err.to_string())?;
        datoms.extend(refs);
    }
    Ok(datoms_value(&datoms))
}

/// Register the native namespace.
pub fn register(registry: &mut Registry) {
    registry.define(
        &format!("{NS}/open"),
        wrap_fn_variadic(
            format!("{NS}/open"),
            1,
            |args: &[Value]| -> Result<Value, String> {
                let path = str_arg(&args[0], "path")?;
                let store = Store::open(Path::new(&path)).map_err(|e| e.to_string())?;
                Ok(Value::NativeObject(gc_native_object(StoreHandle {
                    store: Arc::new(store),
                })))
            },
        ),
    );
    registry.define(
        &format!("{NS}/set-attr!"),
        wrap_fn_variadic(
            format!("{NS}/set-attr!"),
            3,
            |args: &[Value]| -> Result<Value, String> {
                let attr = attr_arg(&args[1], "attribute")?;
                let props = props_from_map(&args[2])?;
                with_store(&args[0], |store| {
                    store.set_attr(&attr, props).map_err(|e| e.to_string())
                })?;
                Ok(Value::Nil)
            },
        ),
    );
    registry.define(
        &format!("{NS}/attrs"),
        wrap_fn1(
            format!("{NS}/attrs"),
            |handle: Value| -> Result<Value, String> {
                with_store(&handle, |store| {
                    let mut m = MapValue::empty();
                    for (name, aid, props) in store.attrs_with_aids() {
                        let mut pm = MapValue::empty();
                        pm = pm.assoc(kw("cardinality-many"), Value::Bool(props.cardinality_many));
                        pm = pm.assoc(kw("ref"), Value::Bool(props.ref_type));
                        pm = pm.assoc(kw("unique-identity"), Value::Bool(props.unique_identity));
                        pm = pm.assoc(kw("unique-value"), Value::Bool(props.unique_value));
                        pm = pm.assoc(kw("aid"), Value::Long(aid as i64));
                        m = m.assoc(Value::keyword(Keyword::parse(&name)), Value::Map(pm));
                    }
                    Ok(Value::Map(m))
                })
            },
        ),
    );
    registry.define(
        &format!("{NS}/transact!"),
        wrap_fn2(
            format!("{NS}/transact!"),
            |handle: Value, ops: Value| -> Result<Value, String> {
                let Value::Vector(items) = ops.unwrap_meta() else {
                    return Err("ops must be a vector".into());
                };
                with_store(&handle, |store| {
                    let mut resolved = Vec::new();
                    for item in items.get().iter() {
                        let Value::Vector(op) = item.unwrap_meta() else {
                            return Err("each op must be [:add|:retract e attr v]".into());
                        };
                        let op = op.get();
                        let parts: Vec<Value> = op.iter().cloned().collect();
                        let [kind, e, a, v] = parts.as_slice() else {
                            return Err("each op must be [:add|:retract e attr v]".into());
                        };
                        let e = eid_arg(e.unwrap_meta(), "entity id")?;
                        let a = attr_arg(a.unwrap_meta(), "attribute")?;
                        let v = to_store_value(store, Some(&a), v)?;
                        let add = matches!(kind.unwrap_meta(), Value::Keyword(k) if k.get().full_name() == "add");
                        resolved.push(if add {
                            Op::Add { e, a, v }
                        } else {
                            Op::Retract { e, a, v }
                        });
                    }
                    store.transact(&resolved).map_err(|e| e.to_string())?;
                    Ok(Value::Long(resolved.len() as i64))
                })
            },
        ),
    );
    registry.define(
        &format!("{NS}/search"),
        wrap_fn_variadic(
            format!("{NS}/search"),
            1,
            |args: &[Value]| -> Result<Value, String> {
                with_store(&args[0], |store| search_impl(store, args))
            },
        ),
    );
    registry.define(
        &format!("{NS}/count"),
        wrap_fn_variadic(
            format!("{NS}/count"),
            1,
            |args: &[Value]| -> Result<Value, String> {
                with_store(&args[0], |store| {
                    let e = opt_eid(args.get(1))?;
                    let a = opt_attr(args.get(2))?;
                    let a_ref = a.as_deref();
                    let sv = match args.get(3).map(Value::unwrap_meta) {
                        None | Some(Value::Nil) => None,
                        Some(val) => Some(to_store_value(store, a_ref, val)?),
                    };
                    let n = store
                        .count(e, a_ref, sv.as_ref())
                        .map_err(|err| err.to_string())?;
                    Ok(Value::Long(n as i64))
                })
            },
        ),
    );
    registry.define(
        &format!("{NS}/sample"),
        wrap_fn_variadic(
            format!("{NS}/sample"),
            3,
            |args: &[Value]| -> Result<Value, String> {
                with_store(&args[0], |store| {
                    let a = attr_arg(&args[1], "attribute")?;
                    let n = eid_arg(args[2].unwrap_meta(), "sample size")?;
                    let ds = store.sample_ave(&a, n).map_err(|err| err.to_string())?;
                    Ok(datoms_value(&ds))
                })
            },
        ),
    );
    registry.define(
        &format!("{NS}/slice"),
        wrap_fn_variadic(
            format!("{NS}/slice"),
            4,
            |args: &[Value]| -> Result<Value, String> {
                with_store(&args[0], |store| slice_impl(store, args, false))
            },
        ),
    );
    registry.define(
        &format!("{NS}/rslice"),
        wrap_fn_variadic(
            format!("{NS}/rslice"),
            4,
            |args: &[Value]| -> Result<Value, String> {
                with_store(&args[0], |store| slice_impl(store, args, true))
            },
        ),
    );
    registry.define(
        &format!("{NS}/count-range"),
        wrap_fn_variadic(
            format!("{NS}/count-range"),
            4,
            |args: &[Value]| -> Result<Value, String> {
                with_store(&args[0], |store| {
                    let index = index_arg(&args[1])?;
                    let (le, la, lv, lc) = bound_parts(store, index, args.get(2))?;
                    let (he, ha, hv, hc) = bound_parts(store, index, args.get(3))?;
                    let low = Bound {
                        e: le,
                        a: la.as_deref(),
                        v: lv.as_ref(),
                        closed: lc,
                    };
                    let high = Bound {
                        e: he,
                        a: ha.as_deref(),
                        v: hv.as_ref(),
                        closed: hc,
                    };
                    let n = store
                        .count_range(index, &low, &high)
                        .map_err(|e| e.to_string())?;
                    Ok(Value::Long(n as i64))
                })
            },
        ),
    );
    registry.define(
        &format!("{NS}/max-eid"),
        wrap_fn1(
            format!("{NS}/max-eid"),
            |handle: Value| -> Result<Value, String> {
                with_store(&handle, |store| {
                    Ok(Value::Long(
                        store.max_eid().map_err(|e| e.to_string())? as i64
                    ))
                })
            },
        ),
    );
    registry.define(
        &format!("{NS}/next-eid"),
        wrap_fn2(
            format!("{NS}/next-eid"),
            |handle: Value, from: Value| -> Result<Value, String> {
                let from = eid_arg(from.unwrap_meta(), "from")?;
                with_store(&handle, |store| {
                    Ok(match store.next_eid(from).map_err(|e| e.to_string())? {
                        Some(e) => Value::Long(e as i64),
                        None => Value::Nil,
                    })
                })
            },
        ),
    );
}
