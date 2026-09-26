//! Wire packet types migrated to `aseman_contracts::legacy_wire::packet`
//! (RL-002); kept as a compatibility re-export shim until the legacy
//! transports retire (RL-009).
//!
//! `signal_tags` semantics are domain-owned and re-exported inline.

pub use aseman_contracts::legacy_wire::packet::*;
pub use aseman_domain::signal_tags::LogQuery;
pub use aseman_domain::signal_tags::*;
