//! Who an operation runs for, as its transport established.

/// Who an operation runs for, as its transport established.
#[derive(Clone, Debug, Default)]
pub struct ActionCaller {
    /// The acting creature, or empty for an anonymous caller.
    pub user_id: String,
    /// The store the guard admitted the caller to (store-guarded operations).
    pub store_id: String,
    /// The node the request came from: this node for a local request, the
    /// submitting node for one ordered on the chain.
    pub source: String,
}