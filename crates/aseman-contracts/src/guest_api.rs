//! The guest API over HTTP (A405 "Host calls"): the paths, the proof header, the
//! workload credential a VMM backend holds for each workload it runs, and request
//! signing.
//!
//! A request is `POST /guest/v1/calls/{op}` with the call's JSON input as the body, or
//! `GET /guest/v1/artifacts/{digest}`. It carries an A401 proof in the `request`
//! context, signed by the workload's `authentication` key, in the `Aseman-Proof` header
//! (unpadded base64url of the proof JSON). The proof's audience is the node's guest
//! audience, its action is the A402 action registered for the call (or
//! [`ARTIFACT_ACTION`]), its resource is the op or digest, and its body digest covers
//! the exact body.

use std::fmt;

use aseman_domain::identity::{CredentialWindow, SignatureContext, Subject, SubjectKind};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};

use crate::identity::{PublicKey, SignedFields, SignedRequestProof, body_digest, sign_ed25519};

pub const PROOF_HEADER: &str = "Aseman-Proof";
pub const CALLS_PATH: &str = "/guest/v1/calls/";
pub const ARTIFACTS_PATH: &str = "/guest/v1/artifacts/";
/// The action a workload signs to read its own program's artifact.
pub const ARTIFACT_ACTION: &str = "workload.artifact.read";
/// Host ops whose `vmId` names the VM operated ON, not the caller. A caller naming
/// another VM sends it in [`TARGET_VM_ID_KEY`]; the node moves it into `vmId` only
/// after resolving and authorizing the caller.
pub const VM_TARGET_OPS: &[&str] = &[
    "runVm",
    "execVm",
    "execDocker",
    "statusVm",
    "terminateVm",
    "deleteVm",
    "destroyVm",
    "copyToVm",
    "copyToDocker",
    "copyFromVm",
    "vmEndpoints",
];
/// Where a VM op's target travels while `vmId` carries the caller.
pub const TARGET_VM_ID_KEY: &str = "targetVmId";
/// Host ops whose `programId` names the program operated ON.
pub const PROGRAM_TARGET_OPS: &[&str] = &[
    "deployEntity",
    "deploy entity",
    "deleteProgram",
    "deleteOwnedProgram",
    "updateProgram",
    "getProgram",
];
/// Where a program op's target travels while `programId` carries the caller.
pub const TARGET_PROGRAM_ID_KEY: &str = "targetProgramId";
/// How long one signed request is valid.
pub const REQUEST_LIFETIME_MILLIS: i64 = 60_000;

/// The A402 action a host call `op` is registered as (`unified-host-call {op}`); the
/// signer and the verifier both derive the signed action from it. `None` for an
/// unregistered call, which is never served.
#[must_use]
pub fn call_action(op: &str) -> Option<&'static str> {
    static SURFACES: std::sync::OnceLock<std::collections::BTreeMap<String, String>> =
        std::sync::OnceLock::new();
    SURFACES
        .get_or_init(|| crate::security::surface_actions().unwrap_or_default())
        .get(&format!("unified-host-call {op}"))
        .map(String::as_str)
}

/// The audience of a node's guest API (A405).
#[must_use]
pub fn audience(node: &Subject) -> String {
    format!("{node}/guest/v1")
}

/// Why a credential could not be used.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidCredential;

impl fmt::Display for InvalidCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid workload credential")
    }
}

impl std::error::Error for InvalidCredential {}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialWire {
    version: u8,
    subject: String,
    key_id: String,
    key_epoch: u32,
    /// Unpadded base64url of the 32-byte Ed25519 seed.
    seed: String,
    /// The node's guest API base URL and its audience.
    guest_api: String,
    audience: String,
}

/// A workload's signing key and where to use it: the write-only `bootstrap.credential`
/// of A501, encoded as unpadded base64url JSON. It never appears in `Debug` output.
pub struct WorkloadCredential {
    pub subject: Subject,
    pub key_id: String,
    pub key_epoch: u32,
    pub guest_api: String,
    pub audience: String,
    seed: [u8; 32],
    key: Ed25519KeyPair,
}

impl fmt::Debug for WorkloadCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WorkloadCredential")
            .field("subject", &self.subject)
            .field("key_id", &self.key_id)
            .field("key_epoch", &self.key_epoch)
            .finish_non_exhaustive()
    }
}

impl WorkloadCredential {
    /// A new key for `workload`.
    ///
    /// # Errors
    ///
    /// When the system has no randomness.
    pub fn generate(
        workload: aseman_domain::WorkloadId,
        guest_api: &str,
        audience: &str,
    ) -> Result<Self, InvalidCredential> {
        let mut seed = [0u8; 32];
        SystemRandom::new()
            .fill(&mut seed)
            .map_err(|_| InvalidCredential)?;
        Self::from_seed(
            Subject {
                kind: SubjectKind::Workload,
                id: *workload.as_uuid(),
            },
            seed,
            1,
            guest_api,
            audience,
        )
    }

    fn from_seed(
        subject: Subject,
        seed: [u8; 32],
        key_epoch: u32,
        guest_api: &str,
        audience: &str,
    ) -> Result<Self, InvalidCredential> {
        let key = Ed25519KeyPair::from_seed_unchecked(&seed).map_err(|_| InvalidCredential)?;
        let public: [u8; 32] = key
            .public_key()
            .as_ref()
            .try_into()
            .map_err(|_| InvalidCredential)?;
        Ok(Self {
            subject,
            key_id: PublicKey::ed25519(public).key_id(),
            key_epoch,
            guest_api: guest_api.trim_end_matches('/').to_owned(),
            audience: audience.to_owned(),
            seed,
            key,
        })
    }

    /// With a different key epoch (after the node registered the key at `epoch`).
    ///
    /// # Errors
    ///
    /// Never for a valid credential.
    pub fn at_epoch(self, epoch: u32) -> Result<Self, InvalidCredential> {
        Self::from_seed(
            self.subject,
            self.seed,
            epoch,
            &self.guest_api,
            &self.audience,
        )
    }

    /// The A401 versioned encoding of the public key, for registration.
    #[must_use]
    pub fn public_key(&self) -> Vec<u8> {
        let public: [u8; 32] = self.key.public_key().as_ref().try_into().unwrap_or([0; 32]);
        PublicKey::ed25519(public).encode()
    }

    /// The write-only text form.
    #[must_use]
    pub fn encode(&self) -> String {
        let wire = CredentialWire {
            version: 1,
            subject: self.subject.to_string(),
            key_id: self.key_id.clone(),
            key_epoch: self.key_epoch,
            seed: URL_SAFE_NO_PAD.encode(self.seed),
            guest_api: self.guest_api.clone(),
            audience: self.audience.clone(),
        };
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&wire).unwrap_or_default())
    }

    /// # Errors
    ///
    /// A malformed credential, or one whose key ID does not match its seed.
    pub fn decode(text: &str) -> Result<Self, InvalidCredential> {
        let bytes = URL_SAFE_NO_PAD
            .decode(text)
            .map_err(|_| InvalidCredential)?;
        let wire: CredentialWire = serde_json::from_slice(&bytes).map_err(|_| InvalidCredential)?;
        if wire.version != 1 {
            return Err(InvalidCredential);
        }
        let seed: [u8; 32] = URL_SAFE_NO_PAD
            .decode(&wire.seed)
            .map_err(|_| InvalidCredential)?
            .try_into()
            .map_err(|_| InvalidCredential)?;
        let subject: Subject = wire.subject.parse().map_err(|_| InvalidCredential)?;
        if subject.kind != SubjectKind::Workload {
            return Err(InvalidCredential);
        }
        let credential = Self::from_seed(
            subject,
            seed,
            wire.key_epoch,
            &wire.guest_api,
            &wire.audience,
        )?;
        if credential.key_id != wire.key_id {
            return Err(InvalidCredential);
        }
        Ok(credential)
    }

    /// Sign one request: `action` and `resource` as A405 names them, `body` exactly as
    /// sent. Returns the `Aseman-Proof` header value.
    ///
    /// # Errors
    ///
    /// When the system has no randomness.
    pub fn sign(
        &self,
        action: &str,
        resource: &str,
        body: &[u8],
        now_millis: i64,
    ) -> Result<String, InvalidCredential> {
        let mut nonce = [0u8; 24];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| InvalidCredential)?;
        let request_id = URL_SAFE_NO_PAD.encode(&nonce[..12]);
        let digest = body_digest(body);
        let proof: SignedRequestProof = sign_ed25519(
            &self.key,
            SignatureContext::Request,
            &SignedFields {
                algorithm: "ed25519",
                key_id: &self.key_id,
                key_epoch: self.key_epoch,
                subject: &self.subject,
                audience: &self.audience,
                window: CredentialWindow {
                    issued_at_millis: now_millis,
                    not_before_millis: now_millis,
                    expires_at_millis: now_millis + REQUEST_LIFETIME_MILLIS,
                },
                nonce: &nonce,
                request_id: &request_id,
                action,
                resource,
                body_digest: &digest,
            },
        );
        Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&proof).unwrap_or_default()))
    }
}

/// Parse an `Aseman-Proof` header into the wire proof.
///
/// # Errors
///
/// When it is not base64url JSON of a proof.
pub fn parse_proof_header(value: &str) -> Result<SignedRequestProof, InvalidCredential> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| InvalidCredential)?;
    serde_json::from_slice(&bytes).map_err(|_| InvalidCredential)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::verify_proof_signature;

    #[test]
    fn a_credential_round_trips_signs_and_never_prints_its_key() {
        let workload = aseman_domain::WorkloadId::new();
        let credential =
            WorkloadCredential::generate(workload, "https://node/", "node:x/guest/v1").unwrap();
        assert_eq!(credential.guest_api, "https://node");
        let text = credential.encode();
        let decoded = WorkloadCredential::decode(&text).unwrap();
        assert_eq!(decoded.key_id, credential.key_id);
        assert_eq!(decoded.public_key(), credential.public_key());
        assert!(!format!("{decoded:?}").contains(&URL_SAFE_NO_PAD.encode(decoded.seed)));
        let header = decoded
            .sign("store.signal", "signal", b"{}", 1_000)
            .unwrap();
        let proof = parse_proof_header(&header).unwrap().parse().unwrap();
        assert_eq!(proof.subject.id, *workload.as_uuid());
        assert_eq!(proof.body_digest, body_digest(b"{}"));
        let key = PublicKey::decode(&decoded.public_key()).unwrap();
        verify_proof_signature(&proof, &key).unwrap();
        // A tampered key ID or subject is refused.
        let mut wire: CredentialWire =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&text).unwrap()).unwrap();
        wire.key_id = "zQmX".to_owned();
        let tampered = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&wire).unwrap());
        assert!(WorkloadCredential::decode(&tampered).is_err());
        assert!(WorkloadCredential::decode("not base64 ***").is_err());
    }

    #[test]
    fn host_calls_sign_their_registered_actions() {
        assert_eq!(call_action("genId"), Some("node.id.generate"));
        assert_eq!(call_action("noSuchCall"), None);
    }
}
