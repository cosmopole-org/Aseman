//! Ports of the guest API (A405): what an authenticated workload's host calls act
//! as, and who serves them.

use aseman_domain::DesiredWorkload;

use crate::PortResult;

/// An authenticated, resolved guest caller: every identity it acts as was derived
/// server-side from the workload record, never from the request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuestCaller {
    pub workload: DesiredWorkload,
    /// The creature's and program's legacy identifiers, which the compatibility host
    /// calls address (ADR 0004).
    pub creature_ref: String,
    pub program_ref: String,
}

impl GuestCaller {
    /// The entity and instance the workload name encodes (`{entity}/{vm}`).
    #[must_use]
    pub fn entity_and_instance(&self) -> (&str, &str) {
        self.workload
            .name
            .split_once('/')
            .unwrap_or((self.workload.name.as_str(), "main"))
    }
}

/// The legacy identifiers of a workload's creature and program.
pub trait LegacyWorkloadRefs: Send + Sync {
    /// `(creature, program)`.
    fn legacy_refs(&self, workload: &DesiredWorkload) -> PortResult<(String, String)>;
}

/// Serves the host calls of resolved guests.
pub trait GuestHostCalls: Send + Sync {
    /// Run host call `op` with the JSON `input` as `caller`; returns the call's JSON
    /// answer. Authorization of the operation is the implementation's (A402).
    fn call(&self, caller: &GuestCaller, op: &str, input: &str) -> PortResult<String>;
    /// The artifact with `digest` (`sha256:{hex}`) of the caller's own program.
    fn artifact(&self, caller: &GuestCaller, digest: &str) -> PortResult<Vec<u8>>;
}
