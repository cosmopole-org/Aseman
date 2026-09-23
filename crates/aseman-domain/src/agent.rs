//! The worker agent's rules (A603, ADR 0010).
//!
//! The agent is the one component with host privilege, so what it may be asked to do
//! is decided here, in values with no filesystem, process, or clock in them.
//!
//! The shape of the trust: mutual TLS says *which component* is calling; a signed
//! **grant** says *what it may do*. A caller with a valid certificate and no grant can
//! do nothing at all.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A machine profile an administrator declared on this host.
///
/// A grant names one of these. It never carries a machine specification of its own,
/// which is what stops a compromised VMM from asking for a microVM with the host's
/// disk attached.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineProfile {
    pub name: String,
    pub vcpu_count: u8,
    pub memory_mib: u64,
    /// The kernel this profile boots. The administrator's, never the caller's.
    pub kernel_image: PathBuf,
    /// The read-only root image.
    pub root_image: PathBuf,
    /// The administrator's network profile name, or none for a machine with no
    /// network at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_profile: Option<String>,
}

/// A short-lived, signed statement of what one caller may do to one allocation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    /// The allocation this grant is about, and the only one it covers.
    pub allocation: String,
    /// The declared profile the machine is created from.
    pub profile: String,
    /// When this grant stops being usable, in milliseconds since the epoch.
    pub expires_at_millis: i64,
    /// The operations it allows.
    pub operations: BTreeSet<AgentOperation>,
}

/// What the agent can be asked to do.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOperation {
    Create,
    Start,
    Pause,
    Resume,
    State,
    Stop,
    Delete,
}

/// What a microVM is actually doing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineState {
    /// Configured, not booted.
    Created,
    Running,
    Paused,
    Stopped,
    /// The process is gone and nobody asked for that.
    Failed,
}

impl MachineState {
    /// Whether `operation` makes sense in this state.
    ///
    /// A pause of a machine that is not running, or a resume of one that is not
    /// paused, is refused rather than quietly turned into something else: the API's
    /// pause means the runtime's pause (A604), and may never degrade to stop/start.
    #[must_use]
    pub fn allows(self, operation: AgentOperation) -> bool {
        match operation {
            AgentOperation::State | AgentOperation::Delete | AgentOperation::Stop => true,
            AgentOperation::Create => false,
            AgentOperation::Start => matches!(self, Self::Created | Self::Stopped),
            AgentOperation::Pause => matches!(self, Self::Running),
            AgentOperation::Resume => matches!(self, Self::Paused),
        }
    }
}

/// Whether `grant` covers `operation` on `allocation` at `now_millis`.
///
/// # Errors
///
/// The reason it does not, as the contract spells it.
pub fn authorize(
    grant: &Grant,
    allocation: &str,
    operation: AgentOperation,
    now_millis: i64,
) -> Result<(), AgentError> {
    if grant.allocation != allocation {
        return Err(AgentError::WrongAllocation);
    }
    if now_millis >= grant.expires_at_millis {
        return Err(AgentError::Expired);
    }
    if !grant.operations.contains(&operation) {
        return Err(AgentError::NotGranted);
    }
    Ok(())
}

/// The directory an allocation's machine owns, under the agent's root.
///
/// The allocation name is the only caller-supplied part, so it is checked rather than
/// cleaned: a name that could escape the root is refused. Normalizing it away would
/// turn an attack into a silent success somewhere unexpected.
///
/// # Errors
///
/// [`AgentError::EscapingPath`] when the allocation is not a plain name.
pub fn allocation_directory(root: &Path, allocation: &str) -> Result<PathBuf, AgentError> {
    let plain = !allocation.is_empty()
        && allocation.len() <= 128
        && allocation
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if !plain {
        return Err(AgentError::EscapingPath);
    }
    Ok(root.join(allocation))
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AgentError {
    #[error("the grant does not cover this allocation")]
    WrongAllocation,
    #[error("the grant has expired")]
    Expired,
    #[error("the grant does not allow this operation")]
    NotGranted,
    #[error("unknown machine profile")]
    UnknownProfile,
    #[error("this host has no KVM")]
    NoKvm,
    #[error("the firecracker capability is disabled on this host")]
    Disabled,
    #[error("the path escapes the allocation root")]
    EscapingPath,
    #[error("unknown allocation")]
    UnknownAllocation,
    #[error("the machine is not in a state that allows this")]
    WrongState,
}

#[cfg(test)]
mod tests;
