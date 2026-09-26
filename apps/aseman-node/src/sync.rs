//! Synchronization helpers for the translated node.

/// Clones a `OnceLock`, preserving an already-initialised value. Used to make
/// translated structs that carry lazy caches cloneable.
pub fn clone_once_lock<T: Clone>(o: &std::sync::OnceLock<T>) -> std::sync::OnceLock<T> {
    let new = std::sync::OnceLock::new();
    if let Some(v) = o.get() {
        let _ = new.set(v.clone());
    }
    new
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_once_lock_preserves_initialised_value() {
        let lock: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        lock.set("hello".to_string()).unwrap();
        let cloned = clone_once_lock(&lock);
        assert_eq!(cloned.get(), Some(&"hello".to_string()));
    }

    #[test]
    fn clone_once_lock_starts_uninitialised_when_empty() {
        let lock: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        let cloned = clone_once_lock(&lock);
        assert!(cloned.get().is_none());
        cloned.set(7).unwrap();
        assert_eq!(cloned.get(), Some(&7));
    }
}
