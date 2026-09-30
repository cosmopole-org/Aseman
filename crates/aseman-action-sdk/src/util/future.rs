//! Detached tasks: run a closure on its own thread, containing a panic.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::thread::JoinHandle;

/// Run `runnable` once on its own thread; a panic ends only that thread.
pub fn async_once<F>(runnable: F) -> JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    std::thread::spawn(move || {
        let _ = catch_unwind(AssertUnwindSafe(runnable));
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn async_once_runs_to_completion() {
        let n = Arc::new(AtomicUsize::new(0));
        let n_clone = n.clone();
        let h = async_once(move || {
            n_clone.fetch_add(1, Ordering::SeqCst);
        });
        h.join().unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn async_once_swallows_panic() {
        let h = async_once(|| panic!("boom"));
        h.join().unwrap();
    }
}