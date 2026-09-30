//! Where a signed-packet request runs.

/// Where a signed-packet request runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionOrigin {
    /// On the node that received it.
    Local,
    /// Ordered on the main chain and run by every node against its own state.
    Replicated,
    /// On the node the request's `origin` field names (this one when empty).
    Requested,
}