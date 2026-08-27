/*
 * transient_map.rs -- implementation of transient maps.
 * Copyright (C) 2026 Casey Marshall
 *
 * This file is licensed under the Eclipse Public License, Version 1,
 * the same license as Clojure.
 */

use crate::hash::{hash_combine_ordered, hash_combine_unordered};
use crate::{ClojureHash, PersistentHashMap, Value, ValueError, ValueResult};
use std::sync::Mutex;

#[derive(Debug)]
pub struct TransientMap {
    map: Mutex<PersistentHashMap>,
    persisted: Mutex<bool>,
}

impl TransientMap {
    pub fn new() -> TransientMap {
        TransientMap {
            map: Mutex::new(PersistentHashMap::empty()),
            persisted: Mutex::new(false),
        }
    }

    pub fn new_from_map(map: &PersistentHashMap) -> TransientMap {
        TransientMap {
            map: Mutex::new(map.clone()),
            persisted: Mutex::new(false),
        }
    }

    pub fn assoc(&self, key: Value, value: Value) -> ValueResult<()> {
        if *self.persisted.lock().unwrap() {
            return Err(ValueError::TransientAlreadyPersisted);
        }
        let mut map = self.map.lock().unwrap();
        *map = map.assoc(key, value);
        Ok(())
    }

    pub fn dissoc(&self, key: &Value) -> ValueResult<()> {
        if *self.persisted.lock().unwrap() {
            return Err(ValueError::TransientAlreadyPersisted);
        }
        let mut map = self.map.lock().unwrap();
        *map = map.dissoc(key);
        Ok(())
    }

    pub fn persistent(&self) -> ValueResult<PersistentHashMap> {
        let map = self.map.lock().unwrap();
        let mut persisted = self.persisted.lock().unwrap();
        if *persisted {
            return Err(ValueError::TransientAlreadyPersisted);
        }
        *persisted = true;
        Ok(map.clone())
    }

    pub fn find(&self, key: &Value) -> Option<(Value, Value)> {
        let map = self.map.lock().unwrap();
        let value = map.get(key);
        value.map(|v| (v.clone(), v.clone()))
    }

    pub fn count(&self) -> usize {
        let map = self.map.lock().unwrap();
        map.count()
    }
}

impl Clone for TransientMap {
    fn clone(&self) -> Self {
        Self {
            map: Mutex::new(self.map.lock().unwrap().clone()),
            persisted: Mutex::new(*self.persisted.lock().unwrap()),
        }
    }
}

impl ClojureHash for TransientMap {
    fn clojure_hash(&self) -> u32 {
        let mut hash: u32 = 0;
        for (k, v) in self.map.lock().unwrap().iter() {
            hash = hash_combine_unordered(
                hash,
                hash_combine_ordered(k.clojure_hash(), v.clojure_hash()),
            );
        }
        hash
    }
}

impl cljrs_gc::Trace for TransientMap {
    fn trace(&self, visitor: &mut cljrs_gc::MarkVisitor) {
        // Delegate to the inner map's trace: it covers index-only key
        // instances that iter() (order-only) misses.
        self.map.lock().unwrap().trace(visitor);
    }

    fn gc_size_extra(&self) -> usize {
        let map = self.map.lock().unwrap();
        map.gc_size_extra()
    }
}

impl Default for TransientMap {
    fn default() -> Self {
        Self::new()
    }
}
