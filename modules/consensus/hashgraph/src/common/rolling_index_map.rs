//! Translation of `chain/common/rolling_index_map.go`.

use std::collections::HashMap;

use anyhow::Result;

use super::rolling_index::RollingIndex;
use super::store_errors::{StoreErrType, new_store_err};

/// A collection of [`RollingIndex`] keyed by participant id.
pub struct RollingIndexMap<T> {
    name: String,
    size: usize,
    keys: Vec<u32>,
    mapping: HashMap<u32, RollingIndex<T>>,
}

impl<T: Clone> RollingIndexMap<T> {
    /// Creates a new `RollingIndexMap` where each `RollingIndex` has the given
    /// size.
    pub fn new(name: &str, size: usize) -> Self {
        RollingIndexMap {
            name: name.to_string(),
            size,
            keys: Vec::new(),
            mapping: HashMap::new(),
        }
    }

    /// Adds a new `RollingIndex` to the map; returns `KeyAlreadyExists` if the
    /// key already exists.
    pub fn add_key(&mut self, key: u32) -> Result<()> {
        if self.mapping.contains_key(&key) {
            return Err(new_store_err(
                &self.name,
                StoreErrType::KeyAlreadyExists,
                &key.to_string(),
            )
            .into());
        }
        self.keys.push(key);
        self.mapping.insert(
            key,
            RollingIndex::new(&format!("{}[{}]", self.name, key), self.size),
        );
        Ok(())
    }

    /// Returns all the items with index greater than `skip_index` from the
    /// `RollingIndex` identified by `key`.
    pub fn get(&self, key: u32, skip_index: i64) -> Result<Vec<T>> {
        let items = self.mapping.get(&key).ok_or_else(|| {
            new_store_err(&self.name, StoreErrType::KeyNotFound, &key.to_string())
        })?;
        items.get(skip_index)
    }

    /// Returns a specific item from a specific `RollingIndex`.
    pub fn get_item(&self, key: u32, index: i64) -> Result<T> {
        let items = self.mapping.get(&key).ok_or_else(|| {
            new_store_err(&self.name, StoreErrType::KeyNotFound, &key.to_string())
        })?;
        items.get_item(index)
    }

    /// Returns the last item from a `RollingIndex` identified by `key`.
    pub fn get_last(&self, key: u32) -> Result<T> {
        let pe = self.mapping.get(&key).ok_or_else(|| {
            new_store_err(&self.name, StoreErrType::KeyNotFound, &key.to_string())
        })?;
        let (cached, _) = pe.get_last_window();
        if cached.is_empty() {
            return Err(new_store_err(&self.name, StoreErrType::Empty, &key.to_string()).into());
        }
        Ok(cached[cached.len() - 1].clone())
    }

    /// Inserts or updates an item into a `RollingIndex` identified by `key`.
    pub fn set(&mut self, key: u32, item: T, index: i64) -> Result<()> {
        let items = self
            .mapping
            .entry(key)
            .or_insert_with(|| RollingIndex::new(&format!("{}[{}]", self.name, key), self.size));
        items.set(item, index)
    }

    /// Returns a mapping of key to last known index.
    pub fn known(&self) -> HashMap<u32, i64> {
        let mut known = HashMap::new();
        for (k, items) in &self.mapping {
            let (_, last_index) = items.get_last_window();
            known.insert(*k, last_index);
        }
        known
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::store_errors::{StoreErrType, is_store};

    #[test]
    fn add_key_rejects_duplicates() {
        let mut m: RollingIndexMap<i64> = RollingIndexMap::new("m", 4);
        m.add_key(1).expect("first add");
        let err = m.add_key(1).expect_err("duplicate add should fail");
        assert!(
            is_store(&err, StoreErrType::KeyAlreadyExists),
            "expected KeyAlreadyExists, got {:?}",
            err
        );
    }

    #[test]
    fn get_for_unknown_key_returns_keynotfound() {
        let m: RollingIndexMap<i64> = RollingIndexMap::new("m", 4);
        let err = m.get(99, -1).expect_err("missing");
        assert!(is_store(&err, StoreErrType::KeyNotFound));
        let err = m.get_item(99, 0).expect_err("missing");
        assert!(is_store(&err, StoreErrType::KeyNotFound));
        let err = m.get_last(99).expect_err("missing");
        assert!(is_store(&err, StoreErrType::KeyNotFound));
    }

    #[test]
    fn set_auto_creates_rolling_index_and_known_tracks_last() {
        let mut m: RollingIndexMap<String> = RollingIndexMap::new("m", 4);
        m.set(1, "a".to_string(), 0).unwrap();
        m.set(1, "b".to_string(), 1).unwrap();
        m.set(2, "x".to_string(), 0).unwrap();

        let known = m.known();
        assert_eq!(known.get(&1), Some(&1));
        assert_eq!(known.get(&2), Some(&0));

        assert_eq!(m.get_item(1, 0).unwrap(), "a");
        assert_eq!(m.get_item(1, 1).unwrap(), "b");
        assert_eq!(m.get_last(1).unwrap(), "b");
        assert_eq!(m.get_last(2).unwrap(), "x");
    }

    #[test]
    fn get_last_on_empty_index_returns_empty() {
        let mut m: RollingIndexMap<i64> = RollingIndexMap::new("m", 4);
        m.add_key(1).unwrap();
        let err = m.get_last(1).expect_err("empty");
        assert!(is_store(&err, StoreErrType::Empty));
    }

    #[test]
    fn get_returns_items_after_skip_index() {
        let mut m: RollingIndexMap<i64> = RollingIndexMap::new("m", 8);
        for i in 0..5 {
            m.set(7, i, i).unwrap();
        }
        let items = m.get(7, 1).unwrap();
        assert_eq!(items, vec![2, 3, 4]);
    }
}
