//! Translation of `chain/common/lru.go` (taken, in turn, from HashiCorp's LRU).
//!
//! Go's implementation uses `container/list`; this translation uses an
//! index-based doubly linked list stored in a slab `Vec`, which gives the same
//! O(1) behaviour without raw pointers.

use std::collections::HashMap;
use std::hash::Hash;

/// Callback invoked when a cache entry is evicted.
pub type EvictCallback<K, V> = Box<dyn Fn(K, V) + Send + Sync>;

struct Node<K, V> {
    key: K,
    value: V,
    /// More-recently-used neighbour (towards the head).
    prev: Option<usize>,
    /// Less-recently-used neighbour (towards the tail).
    next: Option<usize>,
}

/// A non-thread-safe fixed size LRU cache.
pub struct LRU<K, V> {
    size: usize,
    nodes: Vec<Option<Node<K, V>>>,
    free: Vec<usize>,
    /// Most-recently-used entry.
    head: Option<usize>,
    /// Least-recently-used entry.
    tail: Option<usize>,
    items: HashMap<K, usize>,
    on_evict: Option<EvictCallback<K, V>>,
}

impl<K: Eq + Hash + Clone, V: Clone> LRU<K, V> {
    /// Constructs an LRU of the given size.
    pub fn new(size: usize, on_evict: Option<EvictCallback<K, V>>) -> Self {
        LRU {
            size,
            nodes: Vec::new(),
            free: Vec::new(),
            head: None,
            tail: None,
            items: HashMap::new(),
            on_evict,
        }
    }

    fn alloc(&mut self, node: Node<K, V>) -> usize {
        if let Some(idx) = self.free.pop() {
            self.nodes[idx] = Some(node);
            idx
        } else {
            self.nodes.push(Some(node));
            self.nodes.len() - 1
        }
    }

    /// Detaches node `idx` from the linked list (does not free its slot).
    fn unlink(&mut self, idx: usize) {
        let (prev, next) = {
            let n = self.nodes[idx].as_ref().unwrap();
            (n.prev, n.next)
        };
        match prev {
            Some(p) => self.nodes[p].as_mut().unwrap().next = next,
            None => self.head = next,
        }
        match next {
            Some(n) => self.nodes[n].as_mut().unwrap().prev = prev,
            None => self.tail = prev,
        }
        let n = self.nodes[idx].as_mut().unwrap();
        n.prev = None;
        n.next = None;
    }

    /// Inserts an already-allocated node `idx` at the head (most recent).
    fn push_front(&mut self, idx: usize) {
        let old_head = self.head;
        {
            let n = self.nodes[idx].as_mut().unwrap();
            n.prev = None;
            n.next = old_head;
        }
        if let Some(h) = old_head {
            self.nodes[h].as_mut().unwrap().prev = Some(idx);
        }
        self.head = Some(idx);
        if self.tail.is_none() {
            self.tail = Some(idx);
        }
    }

    fn move_to_front(&mut self, idx: usize) {
        if self.head == Some(idx) {
            return;
        }
        self.unlink(idx);
        self.push_front(idx);
    }

    /// Removes node `idx`, frees its slot and fires the evict callback.
    fn remove_element(&mut self, idx: usize) {
        self.unlink(idx);
        let node = self.nodes[idx].take().unwrap();
        self.free.push(idx);
        self.items.remove(&node.key);
        if let Some(cb) = &self.on_evict {
            cb(node.key, node.value);
        }
    }

    /// Completely clears the cache.
    pub fn purge(&mut self) {
        let keys: Vec<usize> = self.items.values().copied().collect();
        for idx in keys {
            if let Some(node) = self.nodes[idx].take() {
                if let Some(cb) = &self.on_evict {
                    cb(node.key, node.value);
                }
            }
        }
        self.nodes.clear();
        self.free.clear();
        self.items.clear();
        self.head = None;
        self.tail = None;
    }

    /// Adds a value to the cache. Returns true if an eviction occurred.
    pub fn add(&mut self, key: K, value: V) -> bool {
        // Check for an existing item.
        if let Some(&idx) = self.items.get(&key) {
            self.move_to_front(idx);
            self.nodes[idx].as_mut().unwrap().value = value;
            return false;
        }

        // Add a new item.
        let idx = self.alloc(Node {
            key: key.clone(),
            value,
            prev: None,
            next: None,
        });
        self.push_front(idx);
        self.items.insert(key, idx);

        let evict = self.items.len() > self.size;
        if evict {
            self.remove_oldest_internal();
        }
        evict
    }

    /// Looks up a key's value, marking it as most-recently-used.
    pub fn get(&mut self, key: &K) -> Option<V> {
        if let Some(&idx) = self.items.get(key) {
            self.move_to_front(idx);
            return Some(self.nodes[idx].as_ref().unwrap().value.clone());
        }
        None
    }

    /// Checks if a key is in the cache without updating recency.
    pub fn contains(&self, key: &K) -> bool {
        self.items.contains_key(key)
    }

    /// Returns a key's value without updating recency.
    pub fn peek(&self, key: &K) -> Option<V> {
        self.items
            .get(key)
            .map(|&idx| self.nodes[idx].as_ref().unwrap().value.clone())
    }

    /// Removes the provided key, returning whether it was present.
    pub fn remove(&mut self, key: &K) -> bool {
        if let Some(&idx) = self.items.get(key) {
            self.remove_element(idx);
            true
        } else {
            false
        }
    }

    fn remove_oldest_internal(&mut self) {
        if let Some(idx) = self.tail {
            self.remove_element(idx);
        }
    }

    /// Removes and returns the oldest item from the cache.
    pub fn remove_oldest(&mut self) -> Option<(K, V)> {
        let idx = self.tail?;
        let (key, value) = {
            let n = self.nodes[idx].as_ref().unwrap();
            (n.key.clone(), n.value.clone())
        };
        self.remove_element(idx);
        Some((key, value))
    }

    /// Returns the oldest entry without removing it.
    pub fn get_oldest(&self) -> Option<(K, V)> {
        self.tail.map(|idx| {
            let n = self.nodes[idx].as_ref().unwrap();
            (n.key.clone(), n.value.clone())
        })
    }

    /// Returns the keys in the cache, from oldest to newest.
    pub fn keys(&self) -> Vec<K> {
        let mut keys = Vec::with_capacity(self.items.len());
        let mut cur = self.tail;
        while let Some(idx) = cur {
            let n = self.nodes[idx].as_ref().unwrap();
            keys.push(n.key.clone());
            cur = n.prev;
        }
        keys
    }

    /// Returns the number of items in the cache.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;

    // Translation of lru_test.go::TestLRU.
    #[test]
    fn test_lru() {
        let evict_counter = Arc::new(AtomicI64::new(0));
        let ec = evict_counter.clone();
        let on_evicted: EvictCallback<i64, i64> = Box::new(move |k, v| {
            assert_eq!(k, v, "Evict values not equal ({}!={})", k, v);
            ec.fetch_add(1, Ordering::SeqCst);
        });
        let mut l: LRU<i64, i64> = LRU::new(128, Some(on_evicted));

        for i in 0..256 {
            l.add(i, i);
        }
        assert_eq!(l.len(), 128, "bad len");
        assert_eq!(evict_counter.load(Ordering::SeqCst), 128, "bad evict count");

        for (i, k) in l.keys().iter().enumerate() {
            let v = l.get(k);
            assert!(v == Some(*k) && v == Some(i as i64 + 128), "bad key: {}", k);
        }
        for i in 0..128 {
            assert!(l.get(&i).is_none(), "should be evicted");
        }
        for i in 128..256 {
            assert!(l.get(&i).is_some(), "should not be evicted");
        }
        for i in 128..192 {
            assert!(l.remove(&i), "should be contained");
            assert!(!l.remove(&i), "should not be contained");
            assert!(l.get(&i).is_none(), "should be deleted");
        }

        l.get(&192); // expect 192 to be the last key in keys()

        for (i, k) in l.keys().iter().enumerate() {
            let i = i as i64;
            assert!(
                !((i < 63 && *k != i + 193) || (i == 63 && *k != 192)),
                "out of order key: {}",
                k
            );
        }

        l.purge();
        assert_eq!(l.len(), 0, "bad len");
        assert!(l.get(&200).is_none(), "should contain nothing");
    }

    // Translation of lru_test.go::TestLRU_GetOldest_RemoveOldest.
    #[test]
    fn test_lru_get_oldest_remove_oldest() {
        let mut l: LRU<i64, i64> = LRU::new(128, None);
        for i in 0..256 {
            l.add(i, i);
        }
        let (k, _) = l.get_oldest().expect("missing");
        assert_eq!(k, 128, "bad");

        let (k, _) = l.remove_oldest().expect("missing");
        assert_eq!(k, 128, "bad");

        let (k, _) = l.remove_oldest().expect("missing");
        assert_eq!(k, 129, "bad");
    }

    // Translation of lru_test.go::TestLRU_Add.
    #[test]
    fn test_lru_add() {
        let evict_counter = Arc::new(AtomicI64::new(0));
        let ec = evict_counter.clone();
        let on_evicted: EvictCallback<i64, i64> = Box::new(move |_, _| {
            ec.fetch_add(1, Ordering::SeqCst);
        });
        let mut l: LRU<i64, i64> = LRU::new(1, Some(on_evicted));

        assert!(
            !(l.add(1, 1) || evict_counter.load(Ordering::SeqCst) != 0),
            "should not have an eviction"
        );
        assert!(
            l.add(2, 2) && evict_counter.load(Ordering::SeqCst) == 1,
            "should have an eviction"
        );
    }

    // Translation of lru_test.go::TestLRU_Contains.
    #[test]
    fn test_lru_contains() {
        let mut l: LRU<i64, i64> = LRU::new(2, None);
        l.add(1, 1);
        l.add(2, 2);
        assert!(l.contains(&1), "1 should be contained");
        l.add(3, 3);
        assert!(
            !l.contains(&1),
            "Contains should not have updated recent-ness of 1"
        );
    }

    // Translation of lru_test.go::TestLRU_Peek.
    #[test]
    fn test_lru_peek() {
        let mut l: LRU<i64, i64> = LRU::new(2, None);
        l.add(1, 1);
        l.add(2, 2);
        assert_eq!(l.peek(&1), Some(1), "1 should be set to 1");
        l.add(3, 3);
        assert!(!l.contains(&1), "should not have updated recent-ness of 1");
    }
}
