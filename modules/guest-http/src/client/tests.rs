//! Over real TLS, with a guest API double that checks proofs with the real A401
//! signature verification and the registered actions.

use std::sync::{Arc, Mutex};

use aseman_application::guest_call::GuestRequest;
use aseman_application::identity::IdentityFailure;
use aseman_contracts::guest_api::{ARTIFACT_ACTION, WorkloadCredential, audience, call_action};
use aseman_contracts::identity::{PublicKey, body_digest, verify_proof_signature};
use aseman_domain::WorkloadId;
use aseman_domain::identity::{AuthenticationError, Proof, Subject, SubjectKind};
use rcgen::{CertificateParams, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

use super::*;
use crate::server::{GuestApi, serve};

struct Node {
    key: PublicKey,
    seen: Mutex<Vec<(String, String)>>,
}

impl GuestApi for Node {
    fn serve(
        &self,
        proof: &Proof,
        body: &[u8],
        request: GuestRequest<'_>,
    ) -> Result<Vec<u8>, IdentityFailure> {
        verify_proof_signature(proof, &self.key)?;
        if proof.body_digest != body_digest(body) {
            return Err(AuthenticationError::BodyDigestMismatch.into());
        }
        match request {
            GuestRequest::Call { op, input } => {
                if Some(proof.action.as_str()) != call_action(op) || proof.resource != op {
                    return Err(IdentityFailure::Refused(
                        "the credential does not cover this operation",
                    ));
                }
                self.seen
                    .lock()
                    .unwrap()
                    .push((op.to_owned(), input.to_owned()));
                Ok(b"{\"ok\":true}".to_vec())
            }
            GuestRequest::Artifact { digest } => {
                if proof.action != ARTIFACT_ACTION || proof.resource != digest {
                    return Err(IdentityFailure::Refused(
                        "the credential does not cover this operation",
                    ));
                }
                if digest == "sha256:missing" {
                    Err(IdentityFailure::Unavailable(PortError::NotFound))
                } else {
                    Ok(b"module bytes".to_vec())
                }
            }
        }
    }
}

#[test]
fn a_backend_calls_the_node_as_its_workload() {
    let key = KeyPair::generate().unwrap();
    let certificate = CertificateParams::new(vec!["127.0.0.1".to_owned()])
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let node = Subject {
        kind: SubjectKind::Node,
        id: aseman_domain::Uuid::from_bytes([1; 16]),
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let url = format!(
        "https://127.0.0.1:{}",
        listener.local_addr().unwrap().port()
    );
    let credential =
        WorkloadCredential::generate(WorkloadId::new(), &url, &audience(&node)).unwrap();
    let api = Arc::new(Node {
        key: PublicKey::decode(&credential.public_key()).unwrap(),
        seen: Mutex::new(Vec::new()),
    });
    let served = api.clone();
    let chain = vec![certificate.der().clone()];
    let private = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    runtime.spawn(async move {
        serve(listener, chain, private, served, std::future::pending())
            .await
            .unwrap();
    });
    let client =
        GuestApiClient::new(certificate.pem().as_bytes(), Duration::from_secs(10)).unwrap();
    assert_eq!(
        client.call(&credential, "genId", b"{}").unwrap(),
        b"{\"ok\":true}"
    );
    assert_eq!(
        api.seen.lock().unwrap()[0],
        ("genId".to_owned(), "{}".to_owned())
    );
    assert_eq!(
        client.artifact(&credential, "sha256:abc").unwrap(),
        b"module bytes"
    );
    assert_eq!(
        client.artifact(&credential, "sha256:missing"),
        Err(PortError::NotFound)
    );
    // An unregistered call is never sent.
    assert_eq!(
        client.call(&credential, "noSuchCall", b"{}"),
        Err(PortError::Unsupported("unregistered host call"))
    );
    // Another workload's key is refused by the node.
    let other = WorkloadCredential::generate(WorkloadId::new(), &url, &audience(&node)).unwrap();
    assert!(matches!(
        client.call(&other, "genId", b"{}"),
        Err(PortError::Failed(message)) if message.starts_with("denied")
    ));
    // The credential's endpoint over plain http is never used.
    let mut wire_plain = WorkloadCredential::decode(&credential.encode()).unwrap();
    wire_plain.guest_api = url.replacen("https", "http", 1);
    assert_eq!(
        client.call(&wire_plain, "genId", b"{}"),
        Err(PortError::Unavailable("guest API"))
    );
}
