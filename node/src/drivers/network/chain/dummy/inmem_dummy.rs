//! Translation of `chain/dummy/inmem_dummy.go`.
//!
//! Wires a [`State`] into an [`InmemProxy`] so the dummy app can be passed
//! directly to the Babble constructor.

use std::sync::Arc;

use crate::compat::logrus::Entry;
use crate::drivers::network::chain::proxy::InmemProxy;

use super::state::State;

/// In-memory dummy client. Holds onto the shared `State` so callers can read
/// out the committed transactions for assertions.
pub struct InmemDummyClient {
    pub proxy: Arc<InmemProxy>,
    pub state: Arc<State>,
}

impl InmemDummyClient {
    /// Instantiate an `InmemDummyClient`.
    pub fn new(logger: Entry) -> Self {
        let state = Arc::new(State::new(logger.clone()));
        let proxy = Arc::new(InmemProxy::new(state.clone(), Some(logger)));
        InmemDummyClient { proxy, state }
    }

    /// Returns the state's list of committed transactions.
    pub fn get_committed_transactions(&self) -> Vec<Vec<u8>> {
        self.state.get_committed_transactions()
    }
}
