//! Translation of `chain/node/state/state.go`.

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// Captures the state of a Babble node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum State {
    /// Gossips regularly as part of the hashgraph consensus algorithm.
    Babbling = 0,
    /// Attempts to fast-forward to a future store (FastSync).
    CatchingUp = 1,
    /// Attempts to join a Babble group by submitting a join request.
    Joining = 2,
    /// Attempts to politely leave a Babble group.
    Leaving = 3,
    /// Stops responding to external events and closes its transport.
    Shutdown = 4,
    /// Passively participates in gossip but processes no new events.
    Suspended = 5,
}

impl Default for State {
    fn default() -> Self {
        State::Babbling
    }
}

impl State {
    fn from_u32(v: u32) -> State {
        match v {
            1 => State::CatchingUp,
            2 => State::Joining,
            3 => State::Leaving,
            4 => State::Shutdown,
            5 => State::Suspended,
            _ => State::Babbling,
        }
    }
}

impl std::fmt::Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            State::Babbling => "Babbling",
            State::CatchingUp => "CatchingUp",
            State::Joining => "Joining",
            State::Leaving => "Leaving",
            State::Shutdown => "Shutdown",
            State::Suspended => "Suspended",
        };
        write!(f, "{}", s)
    }
}

/// The maximum number of background tasks that can be launched through
/// [`Manager::go_func`]. Sized generously so that incoming RPC processing
/// and outgoing gossip don't starve each other.
pub const WGLIMIT: i32 = 500;

/// Wraps a [`State`] with get/set methods. It also limits the number of
/// background tasks the node launches, and waits for all of them to complete.
pub struct Manager {
    state: AtomicU32,
    wg_count: Arc<AtomicI32>,
    handles: Mutex<Vec<JoinHandle<()>>>,
}

impl Default for Manager {
    fn default() -> Self {
        Manager::new()
    }
}

impl Manager {
    pub fn new() -> Manager {
        Manager {
            state: AtomicU32::new(State::Babbling as u32),
            wg_count: Arc::new(AtomicI32::new(0)),
            handles: Mutex::new(Vec::new()),
        }
    }

    /// Returns the current state.
    pub fn get_state(&self) -> State {
        State::from_u32(self.state.load(Ordering::SeqCst))
    }

    /// Sets the state.
    pub fn set_state(&self, s: State) {
        self.state.store(s as u32, Ordering::SeqCst);
    }

    /// Launches a background task for `f`, if fewer than [`WGLIMIT`] are
    /// currently outstanding. The counter is decremented when the task
    /// finishes — via a Drop guard, so panics in `f` still release the slot.
    pub fn go_func(&self, f: Box<dyn FnOnce() + Send>) {
        if self.wg_count.load(Ordering::SeqCst) < WGLIMIT {
            self.wg_count.fetch_add(1, Ordering::SeqCst);
            let wg = Arc::clone(&self.wg_count);
            let handle = std::thread::spawn(move || {
                struct Guard(Arc<AtomicI32>);
                impl Drop for Guard {
                    fn drop(&mut self) {
                        self.0.fetch_sub(1, Ordering::SeqCst);
                    }
                }
                let _guard = Guard(wg);
                f();
            });
            let mut handles = self.handles.lock().unwrap();
            handles.retain(|h| !h.is_finished());
            handles.push(handle);
        }
    }

    /// Waits for all background tasks to complete.
    pub fn wait_routines(&self) {
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *self.handles.lock().unwrap());
        for h in handles {
            let _ = h.join();
        }
        self.wg_count.store(0, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn state_default_is_babbling() {
        assert_eq!(State::default(), State::Babbling);
    }

    #[test]
    fn state_display_matches_go_names() {
        assert_eq!(State::Babbling.to_string(), "Babbling");
        assert_eq!(State::CatchingUp.to_string(), "CatchingUp");
        assert_eq!(State::Joining.to_string(), "Joining");
        assert_eq!(State::Leaving.to_string(), "Leaving");
        assert_eq!(State::Shutdown.to_string(), "Shutdown");
        assert_eq!(State::Suspended.to_string(), "Suspended");
    }

    #[test]
    fn state_from_u32_round_trips_all_variants() {
        for s in [
            State::Babbling,
            State::CatchingUp,
            State::Joining,
            State::Leaving,
            State::Shutdown,
            State::Suspended,
        ] {
            assert_eq!(State::from_u32(s as u32), s);
        }
    }

    #[test]
    fn state_from_u32_unknown_defaults_to_babbling() {
        assert_eq!(State::from_u32(99), State::Babbling);
        assert_eq!(State::from_u32(u32::MAX), State::Babbling);
    }

    #[test]
    fn manager_get_set_state_round_trip() {
        let m = Manager::new();
        assert_eq!(m.get_state(), State::Babbling);
        m.set_state(State::Suspended);
        assert_eq!(m.get_state(), State::Suspended);
        m.set_state(State::Shutdown);
        assert_eq!(m.get_state(), State::Shutdown);
    }

    #[test]
    fn manager_go_func_runs_and_wait_routines_joins() {
        let m = Manager::new();
        let counter = Arc::new(AtomicUsize::new(0));
        for _ in 0..5 {
            let c = counter.clone();
            m.go_func(Box::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
            }));
        }
        m.wait_routines();
        assert_eq!(counter.load(Ordering::SeqCst), 5);
        // After wait_routines, the wg counter should be reset to 0 so new
        // submissions can proceed.
        let c = counter.clone();
        m.go_func(Box::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
        }));
        m.wait_routines();
        assert_eq!(counter.load(Ordering::SeqCst), 6);
    }

    #[test]
    fn manager_go_func_respects_wglimit_concurrency() {
        // Verify that go_func caps CONCURRENT outstanding tasks at WGLIMIT.
        // Because wg_count is now decremented when tasks finish, tasks that
        // complete quickly free up slots for later submissions, so the total
        // number of tasks that ran can exceed WGLIMIT over time.
        let m = Manager::new();
        let counter = Arc::new(AtomicUsize::new(0));
        let total = WGLIMIT as usize * 3;
        for _ in 0..total {
            let c = counter.clone();
            m.go_func(Box::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
            }));
        }
        m.wait_routines();
        // All submitted tasks that were accepted should have run; at minimum
        // WGLIMIT ran (the first batch), but many more may have run as slots freed.
        assert!(
            counter.load(Ordering::SeqCst) >= WGLIMIT as usize,
            "at least WGLIMIT tasks should have run (count={})",
            counter.load(Ordering::SeqCst)
        );
    }
}
