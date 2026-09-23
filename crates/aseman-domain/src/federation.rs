//! Federation: descriptors and envelopes (A704, A705).
//!
//! Two independently administered Aseman clusters talk to each other here. Nomad
//! federation is not this: Nomad connects infrastructure, Aseman federation connects
//! node-clusters and authorizes application-level operations.
//!
//! The rule the rest of this module exists to serve: **resolving an identity never
//! grants authority.** Anyone in the federation may look up a workload's minimal
//! descriptor; every action against it is still authorized at the destination, by the
//! destination, against its own records.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::Uuid;

/// What a node publishes about itself, signed with its node key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDescriptor {
    /// Stable, and bound to the node's public key.
    pub node_id: Uuid,
    /// The key epoch these keys belong to; a rotation raises it.
    pub key_epoch: u32,
    /// Unpadded base64url Ed25519 public keys, newest first.
    pub keys: Vec<String>,
    /// Where other nodes reach this one.
    pub federation_endpoint: String,
    /// Where clients reach it.
    pub client_endpoint: String,
    /// The contract versions it speaks, for example `a501/1`.
    pub contracts: Vec<String>,
    /// The runtime classes it can run.
    pub runtimes: Vec<String>,
    /// Raised on every publication; a cache never accepts a lower one.
    pub sequence: u64,
    pub expires_at_millis: i64,
    /// Key epochs this node has revoked. A descriptor naming a revoked epoch in
    /// `key_epoch` is refused.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revoked_epochs: Vec<u32>,
}

/// The minimum any authenticated workload in the federation may learn about another.
///
/// Deliberately small: it says where to send something and how to verify the answer,
/// and nothing about the creature, the program, its capabilities, its logs, its
/// presence, or its data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadDescriptor {
    pub workload_id: Uuid,
    pub home_node: Uuid,
    pub home_endpoint: String,
    /// Unpadded base64url Ed25519 public key.
    pub public_key: String,
    pub revision: u64,
    pub expires_at_millis: i64,
}

/// One cross-node request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub request_id: Uuid,
    pub source_node: Uuid,
    pub destination_node: Uuid,
    /// Who is asking, as the source authenticated them, for example
    /// `workload:{uuid}`.
    pub subject: String,
    /// What is being acted on.
    pub target: String,
    /// The registered A402 action.
    pub action: String,
    /// `sha256:{hex}` of the payload, so the signature covers the body without
    /// carrying it.
    pub payload_digest: String,
    pub issued_at_millis: i64,
    pub expires_at_millis: i64,
    /// Unique per source node; the destination remembers it until expiry.
    pub nonce: String,
    /// How many more nodes may forward this. Zero means this is the last.
    pub hop_limit: u8,
    /// The envelope contract version, for example `1`.
    pub version: String,
}

/// The most hops an envelope may ever declare. A loop cannot outlive this even if a
/// node misbehaves.
pub const MAX_HOP_LIMIT: u8 = 4;

/// The longest an envelope may be valid for. A long-lived envelope is a replay
/// waiting to happen, and the destination must remember every nonce until expiry.
pub const MAX_LIFETIME_MILLIS: i64 = 60_000;

/// Whether a destination accepts an envelope, before any authorization is attempted.
///
/// This is the destination's own check against its own clock and its own record of
/// what it has seen. It does not decide whether the action is allowed — that is the
/// policy's, afterwards, and it happens at the destination whatever the source
/// believed.
///
/// # Errors
///
/// The reason it is refused.
pub fn accept(
    envelope: &Envelope,
    destination: Uuid,
    now_millis: i64,
    seen_nonce: bool,
) -> Result<(), FederationError> {
    if envelope.version != "1" {
        return Err(FederationError::UnknownVersion);
    }
    if envelope.destination_node != destination {
        // An envelope addressed elsewhere is not this node's to execute, whoever
        // handed it over.
        return Err(FederationError::WrongDestination);
    }
    if envelope.source_node == envelope.destination_node {
        return Err(FederationError::SelfAddressed);
    }
    if envelope.expires_at_millis <= envelope.issued_at_millis {
        return Err(FederationError::Expired);
    }
    if envelope.expires_at_millis - envelope.issued_at_millis > MAX_LIFETIME_MILLIS {
        return Err(FederationError::LifetimeTooLong);
    }
    if now_millis >= envelope.expires_at_millis {
        return Err(FederationError::Expired);
    }
    if envelope.hop_limit > MAX_HOP_LIMIT {
        return Err(FederationError::HopLimitTooHigh);
    }
    if envelope.nonce.is_empty() || envelope.nonce.len() > 128 {
        return Err(FederationError::InvalidNonce);
    }
    if seen_nonce {
        // A repeated nonce is a replay. A retry of the same request carries the same
        // request ID and is answered from the record, not executed again.
        return Err(FederationError::Replayed);
    }
    if !envelope
        .payload_digest
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(FederationError::InvalidDigest);
    }
    if envelope.action.is_empty() || envelope.subject.is_empty() || envelope.target.is_empty() {
        return Err(FederationError::Incomplete);
    }
    Ok(())
}

/// The envelope to forward, one hop further.
///
/// # Errors
///
/// [`FederationError::HopLimitReached`] when it may not be forwarded again.
pub fn forward(envelope: &Envelope) -> Result<Envelope, FederationError> {
    if envelope.hop_limit == 0 {
        return Err(FederationError::HopLimitReached);
    }
    Ok(Envelope {
        hop_limit: envelope.hop_limit - 1,
        ..envelope.clone()
    })
}

/// Whether a cached descriptor may be replaced by a newly received one.
///
/// Sequences never move backwards: a replayed older descriptor cannot un-rotate a key
/// or un-revoke an epoch.
#[must_use]
pub fn accepts_node_descriptor(cached: Option<&NodeDescriptor>, received: &NodeDescriptor) -> bool {
    if received.revoked_epochs.contains(&received.key_epoch) {
        return false;
    }
    cached.is_none_or(|cached| received.sequence > cached.sequence)
}

/// Whether a cached workload descriptor may be replaced.
#[must_use]
pub fn accepts_workload_descriptor(
    cached: Option<&WorkloadDescriptor>,
    received: &WorkloadDescriptor,
) -> bool {
    cached.is_none_or(|cached| received.revision > cached.revision)
}

/// Whether a descriptor may still be used at `now_millis`.
#[must_use]
pub fn descriptor_is_fresh(expires_at_millis: i64, now_millis: i64) -> bool {
    now_millis < expires_at_millis
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FederationError {
    #[error("unknown envelope version")]
    UnknownVersion,
    #[error("this envelope is addressed to another node")]
    WrongDestination,
    #[error("an envelope's source and destination are different nodes")]
    SelfAddressed,
    #[error("the envelope has expired")]
    Expired,
    #[error("an envelope may not be valid for that long")]
    LifetimeTooLong,
    #[error("the hop limit is above the federation maximum")]
    HopLimitTooHigh,
    #[error("the envelope may not be forwarded again")]
    HopLimitReached,
    #[error("the nonce is missing or too long")]
    InvalidNonce,
    #[error("this envelope has already been seen")]
    Replayed,
    #[error("the payload digest is not a sha256 digest")]
    InvalidDigest,
    #[error("the envelope names no subject, target, or action")]
    Incomplete,
}

#[cfg(test)]
mod tests;
