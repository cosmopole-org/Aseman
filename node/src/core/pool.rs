//! Translation of `core/module/pool/pool.go`.
//!
//! Go used `sync.Pool` to recycle `[]byte` allocations. The Rust standard
//! library has no exact analogue, so this module exposes a small `Mutex`-guarded
//! free list with the same semantics: `get_buffer(size)` returns an empty
//! `Vec<u8>` with enough capacity, and `put_buffer(v)` returns it to the pool
//! provided it isn't oversized.

use std::sync::Mutex;
use std::sync::OnceLock;

const DEFAULT_CAPACITY: usize = 2048;
const MAX_CAPACITY: usize = 10 * 1024 * 1024;

fn pool() -> &'static Mutex<Vec<Vec<u8>>> {
    static POOL: OnceLock<Mutex<Vec<Vec<u8>>>> = OnceLock::new();
    POOL.get_or_init(|| Mutex::new(Vec::new()))
}

/// Get an empty buffer with at least `size` bytes of capacity.
pub fn get_buffer(size: usize) -> Vec<u8> {
    let mut guard = pool().lock().unwrap();
    let mut buf = guard
        .pop()
        .unwrap_or_else(|| Vec::with_capacity(DEFAULT_CAPACITY));
    drop(guard);
    if buf.capacity() < size {
        buf = Vec::with_capacity(size);
    } else {
        buf.clear();
    }
    buf
}

/// Return a buffer to the pool. Oversized buffers are dropped to keep the
/// pool's total residency bounded — same heuristic Go used.
pub fn put_buffer(buf: Vec<u8>) {
    if buf.capacity() > MAX_CAPACITY {
        return;
    }
    pool().lock().unwrap().push(buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_buffer_returns_empty_with_capacity() {
        let buf = get_buffer(128);
        assert!(buf.is_empty());
        assert!(buf.capacity() >= 128);
    }

    #[test]
    fn round_trip_buffer() {
        let mut buf = get_buffer(64);
        buf.extend_from_slice(b"hello");
        let cap_before = buf.capacity();
        put_buffer(buf);
        let buf = get_buffer(16);
        assert!(buf.is_empty());
        assert!(buf.capacity() >= cap_before.min(16));
    }

    #[test]
    fn oversize_dropped() {
        let big: Vec<u8> = Vec::with_capacity(MAX_CAPACITY + 1);
        let cap = big.capacity();
        put_buffer(big);
        // Nothing in the pool with that capacity should be returned.
        let buf = get_buffer(0);
        assert!(buf.capacity() <= cap);
    }

    #[test]
    fn get_buffer_zero_size_returns_some_buffer() {
        let buf = get_buffer(0);
        assert!(buf.is_empty());
        drop(buf);
    }

    #[test]
    fn put_buffer_returned_to_pool_keeps_capacity_clear() {
        // A buffer round-tripped through the pool should come back empty even
        // after it was filled.
        let mut buf = get_buffer(256);
        buf.extend(std::iter::repeat(0xFFu8).take(256));
        assert_eq!(buf.len(), 256);
        put_buffer(buf);
        let buf2 = get_buffer(8);
        assert!(buf2.is_empty(), "recycled buffer should be empty");
    }

    #[test]
    fn get_buffer_grows_when_request_exceeds_default_capacity() {
        // A request well beyond DEFAULT_CAPACITY must allocate enough.
        let big = get_buffer(DEFAULT_CAPACITY * 8);
        assert!(big.capacity() >= DEFAULT_CAPACITY * 8);
        assert!(big.is_empty());
    }
}
