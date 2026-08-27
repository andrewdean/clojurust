use crate::Value;
use rpds::{HashTrieMapSync, RedBlackTreeMapSync};

/// An immutable hash set that preserves insertion order when iterated.
///
/// Uses the same index-plus-ordered-log technique as `PersistentHashMap`:
/// a `rpds::HashTrieMap` from value to insertion sequence number, and a
/// `rpds::RedBlackTreeMap` from sequence number to value that is iterated in
/// order. This keeps iteration deterministic and matching insertion order,
/// rather than depending on hash-bucket layout — `rpds` seeds its hasher
/// randomly per instance, so raw hash-order iteration would otherwise vary
/// from run to run.
#[derive(Debug, Clone)]
pub struct PersistentHashSet {
    index: HashTrieMapSync<Value, u64>,
    order: RedBlackTreeMapSync<u64, Value>,
    next_seq: u64,
}

impl PersistentHashSet {
    pub fn empty() -> Self {
        Self {
            index: HashTrieMapSync::new_sync(),
            order: RedBlackTreeMapSync::new_sync(),
            next_seq: 0,
        }
    }

    pub fn count(&self) -> usize {
        self.index.size()
    }

    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn contains(&self, val: &Value) -> bool {
        self.index.contains_key(val)
    }

    /// Return a new set with `val` added.
    ///
    /// If `val` is already present, its original insertion position is kept.
    pub fn conj(&self, val: Value) -> Self {
        if self.index.contains_key(&val) {
            return self.clone();
        }
        let seq = self.next_seq;
        Self {
            index: self.index.insert(val.clone(), seq),
            order: self.order.insert(seq, val),
            next_seq: seq + 1,
        }
    }

    pub fn conj_mut(&mut self, val: Value) -> &mut Self {
        if !self.index.contains_key(&val) {
            let seq = self.next_seq;
            self.index.insert_mut(val.clone(), seq);
            self.order.insert_mut(seq, val);
            self.next_seq = seq + 1;
        }
        self
    }

    /// Return a new set with `val` removed.
    pub fn disj(&self, val: &Value) -> Self {
        match self.index.get(val) {
            Some(seq) => Self {
                index: self.index.remove(val),
                order: self.order.remove(seq),
                next_seq: self.next_seq,
            },
            None => self.clone(),
        }
    }

    /// Iterate over all elements in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        self.order.iter().map(|(_, v)| v)
    }
}

impl FromIterator<Value> for PersistentHashSet {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> Self {
        let mut s = PersistentHashSet::empty();
        for v in iter {
            s.conj_mut(v);
        }
        s
    }
}

impl PartialEq for PersistentHashSet {
    fn eq(&self, other: &Self) -> bool {
        if self.count() != other.count() {
            return false;
        }
        self.iter().all(|k| other.contains(k))
    }
}

impl cljrs_gc::Trace for PersistentHashSet {
    fn trace(&self, visitor: &mut cljrs_gc::MarkVisitor) {
        for (_, v) in self.order.iter() {
            v.trace(visitor);
        }
        // Defense in depth against the PersistentHashMap index-desync bug:
        // conj currently guarantees index keys alias order values, but any
        // future replace-on-insert would leave index-only instances that
        // must still be traced.
        for (k, _) in self.index.iter() {
            k.trace(visitor);
        }
    }

    fn gc_size_extra(&self) -> usize {
        // Per entry: index HashTrieMap entry (~40) + order RedBlackTree node
        // (~48), value stored inline in the order tree (not behind GcPtr).
        let n = self.index.size();
        n * (88 + std::mem::size_of::<Value>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;

    fn int(n: i64) -> Value {
        Value::Long(n)
    }

    #[test]
    fn test_basic() {
        let s = PersistentHashSet::empty();
        let s = s.conj(int(1)).conj(int(2)).conj(int(3));
        assert_eq!(s.count(), 3);
        assert!(s.contains(&int(1)));
        assert!(s.contains(&int(2)));
        assert!(!s.contains(&int(99)));
    }

    #[test]
    fn test_idempotent_conj() {
        let s = PersistentHashSet::empty().conj(int(1)).conj(int(1));
        assert_eq!(s.count(), 1);
    }

    #[test]
    fn test_disj() {
        let s = PersistentHashSet::empty().conj(int(1)).conj(int(2));
        let s2 = s.disj(&int(1));
        assert!(!s2.contains(&int(1)));
        assert!(s2.contains(&int(2)));
        assert_eq!(s2.count(), 1);
    }

    #[test]
    fn test_equality_order_independent() {
        let a = PersistentHashSet::from_iter([int(1), int(2), int(3)]);
        let b = PersistentHashSet::from_iter([int(3), int(1), int(2)]);
        assert_eq!(a, b);
    }

    #[test]
    fn test_iteration_order_matches_insertion_order() {
        let s = PersistentHashSet::empty()
            .conj(int(10))
            .conj(int(3))
            .conj(int(7))
            .conj(int(1));
        let iterated: Vec<Value> = s.iter().cloned().collect();
        assert_eq!(iterated, vec![int(10), int(3), int(7), int(1)]);
    }
}
