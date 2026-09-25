//! Guest-side construction of authenticated A405 HTTP calls.
#![forbid(unsafe_code)]

use aseman_contracts::guest_api::{
    ARTIFACT_ACTION, ARTIFACTS_PATH, CALLS_PATH, PROOF_HEADER, WorkloadCredential, call_action,
};
use thiserror::Error;

pub use aseman_contracts::guest_api::InvalidCredential;

/// A complete request for an HTTP client supplied by the guest application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedGuestRequest {
    pub method: &'static str,
    pub url: String,
    pub proof_header_name: &'static str,
    pub proof_header_value: String,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum GuestSdkError {
    #[error("the guest operation is not registered")]
    UnregisteredOperation,
    #[error("the artifact digest must be 64 lowercase hexadecimal characters")]
    InvalidArtifactDigest,
    #[error(transparent)]
    InvalidCredential(#[from] InvalidCredential),
}

/// A workload-bound client. It never accepts caller-selected tenancy.
pub struct GuestClient {
    credential: WorkloadCredential,
}

impl GuestClient {
    /// Decode the write-only credential injected for this workload.
    ///
    /// # Errors
    ///
    /// The credential is malformed or does not describe a workload key.
    pub fn from_credential(encoded: &str) -> Result<Self, GuestSdkError> {
        Ok(Self {
            credential: WorkloadCredential::decode(encoded)?,
        })
    }

    /// Sign a registered guest operation.
    ///
    /// # Errors
    ///
    /// The operation is absent from A402 or secure randomness is unavailable.
    pub fn call(
        &self,
        operation: &str,
        body: impl Into<Vec<u8>>,
        now_millis: i64,
    ) -> Result<SignedGuestRequest, GuestSdkError> {
        let action = call_action(operation).ok_or(GuestSdkError::UnregisteredOperation)?;
        let body = body.into();
        let proof = self.credential.sign(action, operation, &body, now_millis)?;
        Ok(SignedGuestRequest {
            method: "POST",
            url: format!("{}{CALLS_PATH}{operation}", self.credential.guest_api),
            proof_header_name: PROOF_HEADER,
            proof_header_value: proof,
            content_type: "application/json",
            body,
        })
    }

    /// Sign a content-addressed artifact read.
    ///
    /// # Errors
    ///
    /// The digest is not canonical SHA-256 hex or secure randomness is unavailable.
    pub fn artifact(
        &self,
        digest: &str,
        now_millis: i64,
    ) -> Result<SignedGuestRequest, GuestSdkError> {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(GuestSdkError::InvalidArtifactDigest);
        }
        let body = Vec::new();
        let proof = self
            .credential
            .sign(ARTIFACT_ACTION, digest, &body, now_millis)?;
        Ok(SignedGuestRequest {
            method: "GET",
            url: format!("{}{ARTIFACTS_PATH}{digest}", self.credential.guest_api),
            proof_header_name: PROOF_HEADER,
            proof_header_value: proof,
            content_type: "application/octet-stream",
            body,
        })
    }
}

#[cfg(test)]
mod tests {
    use aseman_contracts::guest_api::{WorkloadCredential, parse_proof_header};
    use aseman_domain::WorkloadId;

    use super::*;

    fn client() -> GuestClient {
        let credential = WorkloadCredential::generate(
            WorkloadId::new(),
            "https://node.example/",
            "node:test/guest/v1",
        )
        .unwrap();
        GuestClient::from_credential(&credential.encode()).unwrap()
    }

    #[test]
    fn registered_calls_bind_the_exact_operation_and_body() {
        let request = client()
            .call("getProgram", br#"{"programId":"p"}"#.to_vec(), 1_000)
            .unwrap();
        assert_eq!(
            request.url,
            "https://node.example/guest/v1/calls/getProgram"
        );
        assert_eq!(request.proof_header_name, "Aseman-Proof");
        let proof = parse_proof_header(&request.proof_header_value)
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(proof.action, call_action("getProgram").unwrap());
        assert_eq!(proof.resource, "getProgram");
        assert_eq!(
            proof.body_digest,
            aseman_contracts::identity::body_digest(&request.body)
        );
    }

    #[test]
    fn unregistered_operations_and_noncanonical_digests_fail_closed() {
        let client = client();
        assert_eq!(
            client.call("chooseDatabase", b"{}".to_vec(), 1_000),
            Err(GuestSdkError::UnregisteredOperation)
        );
        assert_eq!(
            client.artifact(&"A".repeat(64), 1_000),
            Err(GuestSdkError::InvalidArtifactDigest)
        );
    }
}
