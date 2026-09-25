//! Translation of `chain/node/control_timer.go`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, after, bounded, never, select};

/// Produces a timer channel for a given duration.
pub type TimerFactory = Box<dyn Fn(Duration) -> Receiver<Instant> + Send + Sync>;

/// Controls the node's heartbeat.
pub struct ControlTimer {
    timer_factory: TimerFactory,
    /// Signals a tick to the listening process.
    pub tick_rx: Receiver<()>,
    tick_tx: Sender<()>,
    reset_tx: Sender<Duration>,
    reset_rx: Receiver<Duration>,
    stop_tx: Sender<()>,
    stop_rx: Receiver<()>,
    shutdown_tx: Sender<()>,
    shutdown_rx: Receiver<()>,
    is_set: AtomicBool,
    is_shutdown: AtomicBool,
}

impl ControlTimer {
    /// A `ControlTimer` factory method.
    pub fn new(timer_factory: TimerFactory) -> ControlTimer {
        // NOTE: these were originally zero-capacity (rendezvous) channels.
        // That allowed a two-party deadlock: the timer thread blocks in
        // `tick_tx.send(())` waiting for the babble loop to receive a tick,
        // while the babble loop — handling the *previous* tick — blocks in
        // `reset_tx.send(t)` waiting for the timer thread to receive a reset.
        // Neither thread is in its `select!`, so neither rendezvous can ever
        // complete and consensus freezes. A 1-slot buffer makes every signal
        // delivery non-blocking, so each thread always returns to its
        // `select!` and can service the other's message.
        let (tick_tx, tick_rx) = bounded(1);
        let (reset_tx, reset_rx) = bounded(1);
        let (stop_tx, stop_rx) = bounded(1);
        let (shutdown_tx, shutdown_rx) = bounded(1);
        ControlTimer {
            timer_factory,
            tick_rx,
            tick_tx,
            reset_tx,
            reset_rx,
            stop_tx,
            stop_rx,
            shutdown_tx,
            shutdown_rx,
            is_set: AtomicBool::new(false),
            is_shutdown: AtomicBool::new(false),
        }
    }

    /// Creates a `ControlTimer` that ticks at random intervals between `min`
    /// and `2*min`.
    pub fn new_random() -> ControlTimer {
        let random_timeout: TimerFactory = Box::new(|min: Duration| {
            if min.is_zero() {
                return never();
            }
            let min_nanos = min.as_nanos() as u64;
            let extra = (rand::random::<u64>() >> 1) % min_nanos.max(1);
            after(min + Duration::from_nanos(extra))
        });
        ControlTimer::new(random_timeout)
    }

    /// Starts the control timer. Intended to be run on its own thread.
    pub fn run(&self, init: Duration) {
        let set_timer = |t: Duration| -> Receiver<Instant> {
            self.is_set.store(true, Ordering::SeqCst);
            (self.timer_factory)(t)
        };

        let mut timer = set_timer(init);
        loop {
            select! {
                recv(timer) -> _ => {
                    // Non-blocking: if a prior tick is still buffered and
                    // unconsumed, this one coalesces into it (the babble loop
                    // only needs to learn "a tick happened"). Crucially this
                    // never blocks the timer thread, so it always loops back
                    // to `select!` and can service reset/stop/shutdown.
                    let _ = self.tick_tx.try_send(());
                    self.is_set.store(false, Ordering::SeqCst);
                }
                recv(self.reset_rx) -> t => {
                    if let Ok(t) = t {
                        timer = set_timer(t);
                    }
                }
                recv(self.stop_rx) -> _ => {
                    timer = never();
                    self.is_set.store(false, Ordering::SeqCst);
                }
                recv(self.shutdown_rx) -> _ => {
                    self.is_set.store(false, Ordering::SeqCst);
                    return;
                }
            }
        }
    }

    /// Resets the heartbeat timer to a new duration.
    ///
    /// Non-blocking: the babble loop calls this after every tick/gossip cycle,
    /// and must never block here (blocking on a rendezvous send is what caused
    /// the consensus-freeze deadlock). A buffered reset is consumed by the
    /// timer thread on its next `select!`; if one is already pending, dropping
    /// the duplicate is safe because `reset_timer` re-arms on the next cycle.
    pub fn reset(&self, t: Duration) {
        let _ = self.reset_tx.try_send(t);
    }

    /// Stops the heartbeat timer.
    pub fn stop(&self) {
        let _ = self.stop_tx.try_send(());
    }

    /// Returns whether the timer is currently set.
    pub fn is_set(&self) -> bool {
        self.is_set.load(Ordering::SeqCst)
    }

    /// Shuts down the control timer.
    pub fn shutdown(&self) {
        if !self.is_shutdown.swap(true, Ordering::SeqCst) {
            let _ = self.shutdown_tx.send(());
        }
    }
}
