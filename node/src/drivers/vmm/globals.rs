use crate::drivers::vmm::prelude::*;
use crate::models::core::ICore;

// ── Single global entry point ─────────────────────────────────────────────────
//
// `GLOBAL_APP` is the **only** standalone global in the VMM module.
// Everything else is reachable through: `GLOBAL_APP → tools() → vmm() → method`.
//
// VMM submodules (controllers, host-call handlers) must not introduce their
// own global state — they use `with_global_app` to obtain `&ICore` and then
// traverse the service graph as needed.

pub(crate) static GLOBAL_APP: Lazy<Mutex<Option<Arc<dyn ICore>>>> = Lazy::new(|| Mutex::new(None));

pub(crate) fn set_global_app(app: Arc<dyn ICore>) {
    *GLOBAL_APP.lock().unwrap() = Some(app);
}

/// Borrow the global `ICore` handle for the duration of `f`.
///
/// The `GLOBAL_APP` mutex is released **before** `f` is called — the Arc clone
/// is enough to keep the core alive.  This prevents deadlocks when `f` itself
/// needs to acquire other locks.
pub(crate) fn with_global_app<R, F: FnOnce(&Arc<dyn ICore>) -> R>(f: F) -> Option<R> {
    let app = { GLOBAL_APP.lock().unwrap().clone() }?;
    Some(f(&app))
}

// ── Shared data types ─────────────────────────────────────────────────────────
//
// These types are used by the `Vmm` struct fields; they live here so that
// modules which only need the type (not the Vmm impl) can import from one place.

pub(crate) struct ResourceLockState {
    pub(crate) locked: bool,
    pub(crate) owner: Option<String>,
    pub(crate) queue: VecDeque<String>,
}

pub(crate) struct ResourceLockEntry {
    pub(crate) state: Mutex<ResourceLockState>,
    pub(crate) cv: Condvar,
}

/// Registry of per-resource advisory locks backing the `lockResource` /
/// `unlockResource` host calls.
///
/// A lock entry is created lazily on first `acquire` and **reaped once it
/// returns to a fully idle, unreferenced state**. Without the reap, a guest
/// that locks a large or unbounded set of distinct `resource_id`s (the id is an
/// arbitrary guest-supplied string) would pin one `ResourceLockEntry` — a
/// `Mutex` + `Condvar` + queue — per id for the life of the node: an
/// unbounded, guest-driven memory leak.
pub(crate) struct ResourceLockRegistry {
    locks: DashMap<String, Arc<ResourceLockEntry>>,
}

impl ResourceLockRegistry {
    pub(crate) fn new() -> Self {
        Self {
            locks: DashMap::new(),
        }
    }

    /// Number of live lock entries — used by tests (and available for metrics)
    /// to assert the map does not grow without bound.
    pub(crate) fn len(&self) -> usize {
        self.locks.len()
    }

    /// Get or atomically create the lock entry for `resource_id`.
    fn get_or_create(&self, resource_id: &str) -> Arc<ResourceLockEntry> {
        self.locks
            .entry(resource_id.to_string())
            .or_insert_with(|| {
                Arc::new(ResourceLockEntry {
                    state: Mutex::new(ResourceLockState {
                        locked: false,
                        owner: None,
                        queue: VecDeque::new(),
                    }),
                    cv: Condvar::new(),
                })
            })
            .clone()
    }

    /// Acquire `resource_id` for `owner_id`, blocking (FIFO) until it is free.
    /// Re-acquiring a lock already owned by `owner_id` is a no-op success.
    pub(crate) fn acquire(&self, resource_id: &str, owner_id: &str) -> Result<(), String> {
        if resource_id.is_empty() {
            return Err("resourceId is required".to_string());
        }
        if owner_id.is_empty() {
            return Err("ownerId is required".to_string());
        }
        let lock = self.get_or_create(resource_id);
        let mut state = lock.state.lock().unwrap();
        if state.owner.as_deref() == Some(owner_id) {
            return Ok(());
        }
        if state.locked {
            state.queue.push_back(owner_id.to_string());
            loop {
                state = lock.cv.wait(state).unwrap();
                if state.owner.as_deref() == Some(owner_id) {
                    return Ok(());
                }
            }
        }
        state.locked = true;
        state.owner = Some(owner_id.to_string());
        Ok(())
    }

    /// Release `resource_id` held by `owner_id`, handing it to the next waiter
    /// (if any) or clearing it, then reaping the entry if it is now idle and
    /// unreferenced.
    pub(crate) fn release(&self, resource_id: &str, owner_id: &str) -> Result<(), String> {
        if resource_id.is_empty() {
            return Err("resourceId is required".to_string());
        }
        let lock = match self.locks.get(resource_id) {
            Some(l) => l.clone(),
            None => return Err(format!("lock '{}' not found", resource_id)),
        };
        let became_idle;
        {
            let mut state = lock.state.lock().unwrap();
            if state.owner.as_deref() != Some(owner_id) {
                return Err(format!(
                    "lock '{}' not owned by '{}'",
                    resource_id, owner_id
                ));
            }
            if let Some(next) = state.queue.pop_front() {
                state.owner = Some(next);
                became_idle = false;
            } else {
                state.locked = false;
                state.owner = None;
                became_idle = true;
            }
            lock.cv.notify_all();
        }
        // Reap the entry once it is idle AND unreferenced. We drop our own clone
        // first so `strong_count` can fall to 1 — the map's own reference — when
        // no acquirer, holder-in-progress, or waiter remains.
        //
        // `remove_if` evaluates the predicate under the shard's write lock,
        // which blocks a concurrent `get_or_create` from minting a new clone, so
        // `strong_count == 1` is a stable "nobody else references this" test. The
        // `strong_count` guard alone is NOT sufficient: `acquire` returns and
        // drops its `Arc` while the lock stays *held*, so a held lock also has
        // `strong_count == 1` — the idle-state re-check is what prevents reaping
        // (and thereby silently unlocking) a lock that is currently held.
        drop(lock);
        if became_idle {
            self.locks.remove_if(resource_id, |_, e| {
                Arc::strong_count(e) == 1 && {
                    let s = e.state.lock().unwrap();
                    !s.locked && s.owner.is_none() && s.queue.is_empty()
                }
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod resource_lock_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    #[test]
    fn idle_lock_is_reaped_after_release() {
        let reg = ResourceLockRegistry::new();
        reg.acquire("res1", "owner1").unwrap();
        assert_eq!(reg.len(), 1);
        reg.release("res1", "owner1").unwrap();
        // Once fully released with no waiters, the entry must not linger.
        assert_eq!(reg.len(), 0, "idle lock entry must be reaped");
    }

    #[test]
    fn distinct_resource_ids_do_not_accumulate() {
        let reg = ResourceLockRegistry::new();
        // The pathological guest: lock and unlock a large set of distinct ids.
        for i in 0..10_000 {
            let id = format!("res-{i}");
            reg.acquire(&id, "owner").unwrap();
            reg.release(&id, "owner").unwrap();
        }
        assert_eq!(reg.len(), 0, "map must not grow with distinct lock ids");
    }

    #[test]
    fn held_lock_is_not_reaped() {
        let reg = ResourceLockRegistry::new();
        reg.acquire("res1", "owner1").unwrap();
        // A second owner releasing something it does not own is an error and
        // must not reap the entry held by owner1.
        assert!(reg.release("res1", "intruder").is_err());
        assert_eq!(reg.len(), 1, "held lock must survive a bogus release");
        // The real owner releasing does reap it.
        reg.release("res1", "owner1").unwrap();
        assert_eq!(reg.len(), 0);
    }

    #[test]
    fn contended_lock_preserves_mutual_exclusion_and_reaps_at_the_end() {
        // A ref-count-based reap must never drop a lock that a waiter or holder
        // still references. Hammer one resource from many threads and assert no
        // two ever hold it at once, then that it is reaped once quiescent.
        let reg = Arc::new(ResourceLockRegistry::new());
        let concurrent = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for t in 0..8 {
            let reg = reg.clone();
            let concurrent = concurrent.clone();
            let max_seen = max_seen.clone();
            handles.push(thread::spawn(move || {
                let owner = format!("owner-{t}");
                for _ in 0..200 {
                    reg.acquire("hot", &owner).unwrap();
                    let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                    max_seen.fetch_max(now, Ordering::SeqCst);
                    // Tiny critical section to widen the race window.
                    thread::sleep(Duration::from_micros(50));
                    concurrent.fetch_sub(1, Ordering::SeqCst);
                    reg.release("hot", &owner).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            1,
            "lock allowed two holders at once"
        );
        assert_eq!(reg.len(), 0, "lock must be reaped once fully quiescent");
    }
}
