//! Translation of `core/module/actor/model/state/state.go`.

use std::sync::{Arc, Mutex};

use crate::models::info::IInfo;
use crate::models::state::IState;
use crate::models::transaction::ITrx;

/// Concrete state carrier handed to action closures.
///
/// Go's `state.IState.SetInfo` is provided as an inherent method here (the
/// trait doesn't include it — neither does the Go interface) so the shell
/// can re-stamp identity mid-flow if it needs to.
pub struct State {
    inner: Mutex<Inner>,
}

struct Inner {
    info: Option<Arc<dyn IInfo>>,
    trx: Option<Arc<dyn ITrx>>,
    src: String,
}

impl State {
    /// Build a new `State` with all three fields. Pass `None` to skip a
    /// field, equivalent to the variadic `NewState` arg patterns.
    pub fn new(info: Option<Arc<dyn IInfo>>, trx: Option<Arc<dyn ITrx>>, src: &str) -> State {
        State {
            inner: Mutex::new(Inner {
                info,
                trx,
                src: src.to_string(),
            }),
        }
    }

    /// `state.SetInfo(...)` — not part of the trait but kept inherent.
    pub fn set_info(&self, info: Arc<dyn IInfo>) {
        self.inner.lock().unwrap().info = Some(info);
    }
}

impl IState for State {
    fn info(&self) -> Arc<dyn IInfo> {
        self.inner
            .lock()
            .unwrap()
            .info
            .clone()
            .expect("State.info accessed before being set")
    }

    fn trx(&self) -> Arc<dyn ITrx> {
        self.inner
            .lock()
            .unwrap()
            .trx
            .clone()
            .expect("State.trx accessed before being set")
    }

    fn set_trx(&self, trx: Arc<dyn ITrx>) {
        self.inner.lock().unwrap().trx = Some(trx);
    }

    fn source(&self) -> String {
        self.inner.lock().unwrap().src.clone()
    }

    fn set_source(&self, source: &str) {
        self.inner.lock().unwrap().src = source.to_string();
    }
}
