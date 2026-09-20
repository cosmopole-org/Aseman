//! Translation of `chain/common/rolling_index.go`.

use anyhow::Result;

use super::store_errors::{new_store_err, StoreErrType};

/// A FIFO cache that evicts half of its items when it reaches full capacity.
///
/// It contains items in strict sequential order where index is a property of
/// the items. It prevents skipping indexes when inserting new items.
pub struct RollingIndex<T> {
    name: String,
    size: usize,
    last_index: i64,
    items: Vec<T>,
}

impl<T: Clone> RollingIndex<T> {
    /// Creates a new `RollingIndex` that contains up to `size` items.
    pub fn new(name: &str, size: usize) -> Self {
        RollingIndex {
            name: name.to_string(),
            size,
            items: Vec::with_capacity(size),
            last_index: -1,
        }
    }

    /// Returns all the items in the cache and the index of the last item.
    pub fn get_last_window(&self) -> (Vec<T>, i64) {
        (self.items.clone(), self.last_index)
    }

    /// Returns all the items with index greater than `skip_index`, or a
    /// `TooLate` error if some items have been evicted.
    pub fn get(&self, skip_index: i64) -> Result<Vec<T>> {
        if skip_index > self.last_index {
            return Ok(Vec::new());
        }

        let cached_items = self.items.len() as i64;
        // assume there are no gaps between indexes
        let oldest_cached_index = self.last_index - cached_items + 1;
        if skip_index + 1 < oldest_cached_index {
            return Err(
                new_store_err(&self.name, StoreErrType::TooLate, &skip_index.to_string()).into(),
            );
        }

        // index of 'skipped' in RollingIndex
        let start = (skip_index - oldest_cached_index + 1) as usize;
        Ok(self.items[start..].to_vec())
    }

    /// Retrieves an item by index. Returns a `TooLate` error if the item was
    /// evicted, or a `KeyNotFound` error if the item is not found.
    pub fn get_item(&self, index: i64) -> Result<T> {
        let items = self.items.len() as i64;
        let oldest_cached = self.last_index - items + 1;
        if index < oldest_cached {
            return Err(
                new_store_err(&self.name, StoreErrType::TooLate, &index.to_string()).into(),
            );
        }
        let findex = index - oldest_cached;
        if findex >= items {
            return Err(
                new_store_err(&self.name, StoreErrType::KeyNotFound, &index.to_string()).into(),
            );
        }
        Ok(self.items[findex as usize].clone())
    }

    /// Inserts an item and evicts the earlier half of the cache if it reached
    /// full capacity. Returns a `SkippedIndex` error if the item's index is
    /// greater than last index + 1. Setting an item in place is allowed if the
    /// index corresponds to an existing item.
    pub fn set(&mut self, item: T, index: i64) -> Result<()> {
        // only allow setting items with index <= lastIndex + 1 so we may later
        // assume that there are no gaps between items
        if 0 <= self.last_index && index > self.last_index + 1 {
            return Err(
                new_store_err(&self.name, StoreErrType::SkippedIndex, &index.to_string()).into(),
            );
        }

        // adding a new item
        if self.last_index < 0 || index == self.last_index + 1 {
            if self.items.len() >= self.size {
                self.roll();
            }
            self.items.push(item);
            self.last_index = index;
            return Ok(());
        }

        // replace an existing item; make sure index is also >= the oldest
        // cached item's index
        let cached_items = self.items.len() as i64;
        let oldest_cached_index = self.last_index - cached_items + 1;

        if index < oldest_cached_index {
            return Err(
                new_store_err(&self.name, StoreErrType::TooLate, &index.to_string()).into(),
            );
        }

        // replacing existing item
        let position = (index - oldest_cached_index) as usize;
        self.items[position] = item;

        Ok(())
    }

    /// Evicts the earlier half of the cache and shifts the other half down.
    fn roll(&mut self) {
        self.items = self.items.split_off(self.size / 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::network::chain::common::store_errors::{is_store, StoreErrType};

    // Translation of rolling_index_test.go::TestRollingIndex.
    #[test]
    fn test_rolling_index() {
        let size = 20usize;
        let test_size = 3 * size / 2;
        let mut rolling_index: RollingIndex<String> = RollingIndex::new("test", size);
        let mut items: Vec<String> = Vec::new();
        for i in 0..test_size {
            let item = format!("item{}", i);
            rolling_index.set(item.clone(), i as i64).unwrap();
            items.push(item);
        }
        let (cached, last_index) = rolling_index.get_last_window();

        let expected_last_index = test_size as i64 - 1;
        assert_eq!(last_index, expected_last_index, "lastIndex mismatch");

        let start = ((last_index / size as i64) * size as i64 / 2) as usize;
        let count = cached.len();
        for i in 0..count {
            assert_eq!(cached[i], items[start + i], "cached[{}] mismatch", i);
        }

        let err = rolling_index
            .set("ErrSkippedIndex".to_string(), expected_last_index + 2)
            .unwrap_err();
        assert!(
            is_store(&err, StoreErrType::SkippedIndex),
            "Should return SkippedIndex"
        );

        let err = rolling_index.get_item(9).unwrap_err();
        assert!(
            is_store(&err, StoreErrType::TooLate),
            "Should return TooLate"
        );

        for &i in &[10i64, 17, 29] {
            let item = rolling_index.get_item(i).unwrap();
            assert_eq!(item, items[i as usize], "GetItem error");
        }

        let err = rolling_index.get_item(last_index + 1).unwrap_err();
        assert!(
            is_store(&err, StoreErrType::KeyNotFound),
            "Should return KeyNotFound"
        );

        // Test updating an item in place.
        let update_index = 26i64;
        let update_value = "Updated Item".to_string();
        rolling_index
            .set(update_value.clone(), update_index)
            .unwrap();
        let item = rolling_index.get_item(update_index).unwrap();
        assert_eq!(item, update_value, "Updated item mismatch");
    }

    // Translation of rolling_index_test.go::TestRollingIndexSkip.
    #[test]
    fn test_rolling_index_skip() {
        let size = 20usize;
        let test_size = 25usize;
        let mut rolling_index: RollingIndex<String> = RollingIndex::new("test", size);

        rolling_index.get(-1).expect("Get(-1) on empty");

        let mut items: Vec<String> = Vec::new();
        for i in 0..test_size {
            let item = format!("item{}", i);
            rolling_index.set(item.clone(), i as i64).unwrap();
            items.push(item);
        }

        match rolling_index.get(0) {
            Ok(_) => {}
            Err(e) => assert!(
                is_store(&e, StoreErrType::TooLate),
                "Skipping index 0 should return TooLate"
            ),
        }

        let skip_index1 = 9usize;
        let expected1 = &items[skip_index1 + 1..];
        let cached1 = rolling_index.get(skip_index1 as i64).unwrap();
        assert_eq!(
            cached1.as_slice(),
            expected1,
            "expected1 and cached not equal"
        );

        let skip_index2 = 15usize;
        let expected2 = &items[skip_index2 + 1..];
        let cached2 = rolling_index.get(skip_index2 as i64).unwrap();
        assert_eq!(
            cached2.as_slice(),
            expected2,
            "expected2 and cached not equal"
        );

        let skip_index3 = 27usize;
        let cached3 = rolling_index.get(skip_index3 as i64).unwrap();
        assert!(cached3.is_empty(), "expected3 and cached not equal");
    }
}
