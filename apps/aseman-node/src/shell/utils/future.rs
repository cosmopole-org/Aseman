//! Translation of `shell/utils/future/future.go`.
//!
//! Go's `future.Async(f, retriable)` spawned a goroutine that ran `f` once,
//! or in an infinite restart loop when `retriable == true`. The Rust port
//! uses `std::thread::spawn`; the loop is reinitialised on panic via
//! `std::panic::catch_unwind` so a panic doesn't terminate the loop (Go's
//! `recover()` did the equivalent in the commented-out blocks).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::thread::JoinHandle;

/// Spawn `runnable` on a background thread. If `retriable` is `true`, the
/// thread restarts the runnable in a loop, swallowing panics so a single
/// crash doesn't terminate the loop.
pub fn r#async<F>(runnable: F, retriable: bool) -> JoinHandle<()>
where
    F: Fn() + Send + 'static,
{
    if retriable {
        std::thread::spawn(move || {
            loop {
                let _ = catch_unwind(AssertUnwindSafe(|| runnable()));
            }
        })
    } else {
        std::thread::spawn(move || {
            let _ = catch_unwind(AssertUnwindSafe(runnable));
        })
    }
}

/// Spawn `runnable` once on a background thread. Convenience wrapper around
/// [`r#async`] for one-shot tasks (the non-retriable Go form).
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

    #[test]
    fn async_non_retriable_runs_once() {
        let n = Arc::new(AtomicUsize::new(0));
        let n_clone = n.clone();
        let h = r#async(
            move || {
                n_clone.fetch_add(1, Ordering::SeqCst);
            },
            false,
        );
        h.join().unwrap();
        assert_eq!(n.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn async_retriable_restarts_on_panic_until_observable_progress() {
        // The closure panics until it has been entered N times, then sleeps
        // forever on a parking lot to prove the loop survived the panics.
        let n = Arc::new(AtomicUsize::new(0));
        let target: usize = 4;
        let n_clone = n.clone();

        let _handle = r#async(
            move || {
                let count = n_clone.fetch_add(1, Ordering::SeqCst) + 1;
                if count < target {
                    panic!("intentional panic #{}", count);
                }
                // Stop work by sleeping so subsequent iterations don't spin
                // the CPU. The test only cares that we reached `target`.
                std::thread::sleep(std::time::Duration::from_secs(60));
            },
            true,
        );

        // Wait up to ~2s for the counter to reach the target.
        for _ in 0..200 {
            if n.load(Ordering::SeqCst) >= target {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            n.load(Ordering::SeqCst) >= target,
            "retriable loop did not survive panics (n={})",
            n.load(Ordering::SeqCst)
        );
        // The detached thread continues sleeping until the test process exits;
        // intentional, since the JoinHandle for an infinite loop never returns.
    }
}
