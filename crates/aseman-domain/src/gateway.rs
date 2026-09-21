//! Gateway route values shared by the route use cases and their adapters.

use serde::{Deserialize, Serialize};

/// A creature's custom HTTP route to one of its program entities (legacy
/// `vmHttpRoute`, target `core.gateway_route`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct GatewayRoute {
    /// The creature whose username or id addresses the route.
    pub creature_id: String,
    /// The normalized route prefix.
    pub path: String,
    pub program_id: String,
    pub entity_id: String,
    pub runtime: String,
    /// A pinned VM instance, or empty. A pin is observed runtime state (ADR 0022):
    /// the legacy provider keeps it, and the capsule provider does not persist it.
    pub pinned_vm_id: String,
}
