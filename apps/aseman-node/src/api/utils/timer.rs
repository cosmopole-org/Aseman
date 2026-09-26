//! Translation of `shell/utils/timer.go`.
//!
//! `schedule(p, o, f, cancel)` repeatedly invokes `f` aligned to a period `p`
//! offset by `o`. Returns when the `cancel` receiver disconnects, matching
//! Go's `ctx.Done()` semantics.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, select, tick};

/// Periodic scheduler. `period` is the cadence, `offset` shifts the first
/// firing relative to the wall-clock period boundary, and `f` is invoked with
/// the firing instant. Returns when `cancel` is closed.
pub fn schedule<F>(period: Duration, offset: Duration, mut f: F, cancel: Receiver<()>)
where
    F: FnMut(SystemTime) + Send,
{
    // Align first firing to the next period boundary + offset.
    let now = SystemTime::now();
    let elapsed = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let period_ns = period.as_nanos().max(1);
    let truncated = elapsed - (elapsed % period_ns);
    let mut first_ns = truncated + offset.as_nanos();
    if first_ns < elapsed {
        first_ns += period_ns;
    }
    let first_wait = Duration::from_nanos((first_ns - elapsed) as u64);

    let first_ch = tick(first_wait);

    // First firing.
    select! {
        recv(first_ch) -> _ => {
            f(SystemTime::now());
        }
        recv(cancel) -> _ => return,
    }

    // Periodic firings.
    let tick_ch = tick(period);
    loop {
        select! {
            recv(tick_ch) -> _ => f(SystemTime::now()),
            recv(cancel) -> _ => return,
        }
    }
}

/// Tiny wrapper that lets a caller report wall-clock time using `Instant`
/// instead of `SystemTime`, matching the Go test helper.
#[allow(dead_code)]
fn _start_instant() -> Instant {
    Instant::now()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn schedule_returns_when_cancel_is_closed_before_first_tick() {
        // Long period — first firing would be ~1s away.
        let (tx, rx) = unbounded::<()>();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let handle = std::thread::spawn(move || {
            schedule(
                Duration::from_secs(1),
                Duration::ZERO,
                move |_| {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                },
                rx,
            );
        });

        // Drop the sender immediately; schedule must return when cancel
        // disconnects (no firings expected).
        drop(tx);

        handle
            .join()
            .expect("scheduler thread should exit cleanly on cancel");
        assert_eq!(counter.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn schedule_fires_at_least_once_then_cancels() {
        // Very short period so the first firing happens almost immediately.
        let (tx, rx) = unbounded::<()>();
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let handle = std::thread::spawn(move || {
            schedule(
                Duration::from_millis(20),
                Duration::ZERO,
                move |_| {
                    counter_clone.fetch_add(1, Ordering::SeqCst);
                },
                rx,
            );
        });

        // Allow several firings to happen.
        std::thread::sleep(Duration::from_millis(120));
        drop(tx);
        handle.join().expect("clean exit");

        assert!(
            counter.load(Ordering::SeqCst) >= 1,
            "scheduler should have fired at least once"
        );
    }
}
