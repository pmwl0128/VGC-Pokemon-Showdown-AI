//! An insertion-ordered map, for the tracked state whose iteration order the v3 encoder relies on
//! (`Object.entries(mon.volatiles).slice(-4)` and the like). Updating an existing key keeps its
//! place, matching a JS object's property order; removal drops it. Values are looked up by a
//! `BTreeMap` so keys must be unique.
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default)]
pub struct Omap<V> {
    order: Vec<String>,
    map: BTreeMap<String, V>,
}

impl<V> Omap<V> {
    pub fn new() -> Self {
        Omap { order: Vec::new(), map: BTreeMap::new() }
    }

    pub fn insert(&mut self, key: String, value: V) {
        if self.map.insert(key.clone(), value).is_none() {
            self.order.push(key);
        }
    }

    pub fn get(&self, key: &str) -> Option<&V> {
        self.map.get(key)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.map.get_mut(key)
    }

    pub fn remove(&mut self, key: &str) -> Option<V> {
        let out = self.map.remove(key);
        if out.is_some() {
            self.order.retain(|k| k != key);
        }
        out
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.map.contains_key(key)
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn clear(&mut self) {
        self.order.clear();
        self.map.clear();
    }

    /// Entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
        self.order.iter().filter_map(move |k| self.map.get(k).map(|v| (k, v)))
    }

    /// Drop every entry the predicate rejects, keeping the order of the rest.
    pub fn retain(&mut self, mut keep: impl FnMut(&String, &V) -> bool) {
        self.order.retain(|k| self.map.get(k).map(|v| keep(k, v)).unwrap_or(false));
        let order = &self.order;
        self.map.retain(|k, _| order.contains(k));
    }
}

impl<V> FromIterator<(String, V)> for Omap<V> {
    fn from_iter<T: IntoIterator<Item = (String, V)>>(iter: T) -> Self {
        let mut out = Omap::new();
        for (k, v) in iter {
            out.insert(k, v);
        }
        out
    }
}
