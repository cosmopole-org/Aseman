//! A603 worker-agent HTTP wire values and signing bytes.
//!
//! The executable verifies these values, while VMM-side callers construct them. Keeping
//! the encoding here gives both sides one owner for the cryptographic wire contract.

use aseman_domain::agent::{AgentOperation, Grant, MachineState};
use serde::{Deserialize, Serialize};

/// Domain separation for an A603 v1 grant signature, including its trailing NUL.
pub const GRANT_SIGNATURE_DOMAIN: &[u8] = b"aseman-vmm-agent-grant-v1\0";

/// A short-lived VMM authorization presented to the privileged worker agent.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedGrant {
    pub key_epoch: u64,
    pub grant: Grant,
    /// Unpadded base64url Ed25519 signature over [`grant_message`].
    pub signature: String,
}

/// Successful A603 operation response.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentAnswer {
    pub allocation: String,
    pub operation: AgentOperation,
    pub state: MachineState,
}

/// Exact bytes covered by the A603 signature.
///
/// Serde emits fields in declaration order. `Grant.operations` is a `BTreeSet`, so the
/// nested operation list is deterministic as well.
///
/// # Errors
///
/// If the contract value cannot be encoded as JSON.
pub fn grant_message(key_epoch: u64, grant: &Grant) -> Result<Vec<u8>, serde_json::Error> {
    #[derive(Serialize)]
    struct Signed<'a> {
        key_epoch: u64,
        grant: &'a Grant,
    }

    let mut message = GRANT_SIGNATURE_DOMAIN.to_vec();
    message.extend_from_slice(&serde_json::to_vec(&Signed { key_epoch, grant })?);
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signing_bytes_are_domain_separated_and_stable() {
        let grant = Grant {
            allocation: "alloc-one".to_owned(),
            profile: "small".to_owned(),
            expires_at_millis: 42,
            operations: [AgentOperation::Start, AgentOperation::State]
                .into_iter()
                .collect(),
        };
        let message = grant_message(3, &grant).unwrap();
        assert!(message.starts_with(b"aseman-vmm-agent-grant-v1\0{"));
        assert_eq!(message[GRANT_SIGNATURE_DOMAIN.len() - 1], 0);
        assert_eq!(
            &message[GRANT_SIGNATURE_DOMAIN.len()..],
            br#"{"key_epoch":3,"grant":{"allocation":"alloc-one","profile":"small","expires_at_millis":42,"operations":["start","state"]}}"#
        );
    }
}
