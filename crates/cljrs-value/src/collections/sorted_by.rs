//! Sorted collections with a user-supplied comparator (`sorted-map-by`,
//! `sorted-set-by`).
//!
//! The comparator is a Clojure fn `Value`; this crate cannot invoke it
//! (callbacks live above cljrs-value), so ordering-sensitive operations take
//! a `cmp` closure supplied by the caller (the builtins layer wraps
//! `callback::invoke`). Order-insensitive operations (`get`, `contains`,
//! `dissoc`) use a linear scan with standard `=` equality — a deliberate
//! divergence from Clojure, which uses comparator-equality for those too;
//! the behaviors agree whenever the comparator is consistent with `=`.

use std::cmp::Ordering;

use crate::Value;
use crate::error::ValueResult;

/// Comparator callback: total order over values, may fail (bad comparator).
pub type CmpFn<'a> = &'a mut dyn FnMut(&Value, &Value) -> ValueResult<Ordering>;

#[derive(Debug, Clone)]
pub struct SortedByMap {
    /// The Clojure comparator fn, kept so `assoc`/`conj` on the result can
    /// keep ordering with it.
    pub comparator: Value,
    /// Entries, kept sorted by the comparator.
    pairs: Vec<(Value, Value)>,
}

impl SortedByMap {
    pub fn new(comparator: Value) -> Self {
        Self {
            comparator,
            pairs: Vec::new(),
        }
    }

    pub fn count(&self) -> usize {
        self.pairs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Value, &Value)> {
        self.pairs.iter().map(|(k, v)| (k, v))
    }

    pub fn keys(&self) -> Vec<Value> {
        self.pairs.iter().map(|(k, _)| k.clone()).collect()
    }

    pub fn vals(&self) -> Vec<Value> {
        self.pairs.iter().map(|(_, v)| v.clone()).collect()
    }

    pub fn get(&self, key: &Value) -> Option<&Value> {
        self.pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn contains_key(&self, key: &Value) -> bool {
        self.pairs.iter().any(|(k, _)| k == key)
    }

    pub fn dissoc(&self, key: &Value) -> Self {
        Self {
            comparator: self.comparator.clone(),
            pairs: self
                .pairs
                .iter()
                .filter(|(k, _)| k != key)
                .cloned()
                .collect(),
        }
    }

    /// Rebuild from pairs that are already in comparator order.
    pub fn from_sorted_pairs(comparator: Value, pairs: Vec<(Value, Value)>) -> Self {
        Self { comparator, pairs }
    }

    /// Insert (or replace, on comparator-equal key) via binary search.
    pub fn assoc_with(&self, key: Value, value: Value, cmp: CmpFn) -> ValueResult<Self> {
        let mut pairs = self.pairs.clone();
        match binary_search(pairs.len(), |i| cmp(&pairs[i].0, &key))? {
            Ok(i) => pairs[i] = (key, value),
            Err(i) => pairs.insert(i, (key, value)),
        }
        Ok(Self {
            comparator: self.comparator.clone(),
            pairs,
        })
    }
}

impl PartialEq for SortedByMap {
    fn eq(&self, other: &Self) -> bool {
        self.count() == other.count() && self.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

impl cljrs_gc::Trace for SortedByMap {
    fn trace(&self, visitor: &mut cljrs_gc::MarkVisitor) {
        self.comparator.trace(visitor);
        for (k, v) in &self.pairs {
            k.trace(visitor);
            v.trace(visitor);
        }
    }

    fn gc_size_extra(&self) -> usize {
        self.pairs.capacity() * 2 * std::mem::size_of::<Value>()
    }
}

#[derive(Debug, Clone)]
pub struct SortedBySet {
    pub comparator: Value,
    items: Vec<Value>,
}

impl SortedBySet {
    pub fn new(comparator: Value) -> Self {
        Self {
            comparator,
            items: Vec::new(),
        }
    }

    pub fn count(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        self.items.iter()
    }

    pub fn contains(&self, value: &Value) -> bool {
        self.items.iter().any(|x| x == value)
    }

    /// Order-blind fallback insert (used where no comparator callback is
    /// available): appends unless an `=`-equal element is present.
    pub fn push_if_absent(&mut self, value: Value) {
        if !self.contains(&value) {
            self.items.push(value);
        }
    }

    pub fn disj(&self, value: &Value) -> Self {
        Self {
            comparator: self.comparator.clone(),
            items: self.items.iter().filter(|x| *x != value).cloned().collect(),
        }
    }

    /// Insert via binary search; a comparator-equal element is replaced
    /// (matching Clojure, where the set keeps the FIRST element — so an
    /// already-present element is left in place).
    pub fn conj_with(&self, value: Value, cmp: CmpFn) -> ValueResult<Self> {
        let mut items = self.items.clone();
        match binary_search(items.len(), |i| cmp(&items[i], &value))? {
            Ok(_) => {} // comparator-equal element already present
            Err(i) => items.insert(i, value),
        }
        Ok(Self {
            comparator: self.comparator.clone(),
            items,
        })
    }
}

impl PartialEq for SortedBySet {
    fn eq(&self, other: &Self) -> bool {
        self.count() == other.count() && self.iter().all(|x| other.contains(x))
    }
}

impl cljrs_gc::Trace for SortedBySet {
    fn trace(&self, visitor: &mut cljrs_gc::MarkVisitor) {
        self.comparator.trace(visitor);
        for x in &self.items {
            x.trace(visitor);
        }
    }

    fn gc_size_extra(&self) -> usize {
        self.items.capacity() * std::mem::size_of::<Value>()
    }
}

/// Fallible binary search: `probe(i)` compares element `i` to the needle.
/// Ok(i) = comparator-equal element at `i`; Err(i) = insertion point.
fn binary_search(
    len: usize,
    mut probe: impl FnMut(usize) -> ValueResult<Ordering>,
) -> ValueResult<Result<usize, usize>> {
    let (mut lo, mut hi) = (0usize, len);
    while lo < hi {
        let mid = (lo + hi) / 2;
        match probe(mid)? {
            Ordering::Less => lo = mid + 1,
            Ordering::Greater => hi = mid,
            Ordering::Equal => return Ok(Ok(mid)),
        }
    }
    Ok(Err(lo))
}
