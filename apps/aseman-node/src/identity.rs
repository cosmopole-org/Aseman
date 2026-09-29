//! The node's keys and creature signatures: per-tag RSA key pairs under
//! `<storage_root>/keys/<tag>/`, PSS-SHA256 verification against a creature's
//! stored public key, and store membership checks.

use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use rsa::pkcs1v15::{Signature as Pkcs1Signature, VerifyingKey as Pkcs1VerifyingKey};
use rsa::pss::{Signature as PssSignature, VerifyingKey};
use rsa::sha2::Sha256;
use rsa::signature::Verifier;

use crate::node::Node;
use crate::util::crypto as cryp;

const KEYS_FOLDER: &str = "keys";

/// The node's keys and signature verification.
pub struct Security {
    app: Arc<Node>,
    storage_root: String,
    keys: Mutex<HashMap<String, Vec<Vec<u8>>>>,
}

impl Security {
    /// `New(core, storageRoot, storage, signaler)`.
    pub fn new(app: Arc<Node>, storage_root: &str) -> Arc<Security> {
        let s = Arc::new(Security {
            app,
            storage_root: storage_root.to_string(),
            keys: Mutex::new(HashMap::new()),
        });
        s.load_keys();
        s
    }
}

impl Security {
    pub(crate) fn load_keys(&self) {
        let dir = format!("{}/{}", self.storage_root, KEYS_FOLDER);
        if let Ok(read) = fs::read_dir(&dir) {
            let mut keys = self.keys.lock().unwrap();
            for entry in read.flatten() {
                if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let priv_path = format!("{}/{}/private.pem", dir, name);
                let pub_path = format!("{}/{}/public.pem", dir, name);
                match (fs::read(&priv_path), fs::read(&pub_path)) {
                    (Ok(priv_k), Ok(pub_k)) => {
                        keys.insert(name, vec![priv_k, pub_k]);
                    }
                    _ => continue,
                }
            }
        }

        if self.fetch_key_pair("server_key").is_empty() {
            self.generate_secure_key_pair("server_key");
        }
    }

    pub(crate) fn generate_secure_key_pair(&self, tag: &str) {
        let dir = format!("{}/{}/{}", self.storage_root, KEYS_FOLDER, tag);
        match cryp::secure_key_pairs(&dir) {
            Ok((priv_k, pub_k)) => {
                self.keys
                    .lock()
                    .unwrap()
                    .insert(tag.to_string(), vec![priv_k, pub_k]);
            }
            Err(e) => eprintln!("generate_secure_key_pair: {}", e),
        }
    }

    pub(crate) fn fetch_key_pair(&self, tag: &str) -> Vec<Vec<u8>> {
        self.keys
            .lock()
            .unwrap()
            .get(tag)
            .cloned()
            .unwrap_or_default()
    }
    pub(crate) fn auth_with_signature(
        &self,
        user_id: &str,
        packet: &[u8],
        signature_base64: &str,
    ) -> (bool, String, bool) {
        // The creature record is the single authoritative identity: its public key
        // verifies the signature and its type is returned on success.
        let creature = self.app.read(|trx| {
            aseman_ports::CreatureDirectory::creature(
                &crate::state::creature_ports::CreaturePorts { trx },
                user_id,
            )
            .map_err(|error| anyhow::anyhow!("{error}"))
        });
        let Ok(Some(creature)) = creature else {
            return (false, String::new(), false);
        };
        let pub_key = match cryp::parse_public_key(creature.public_key.as_bytes()) {
            Ok(key) => key,
            Err(_) => return (false, String::new(), false),
        };

        let signature = match B64.decode(signature_base64) {
            Ok(s) => s,
            Err(_) => return (false, String::new(), false),
        };
        // Primary scheme: RSA-PSS-SHA256 (what the SDKs and the CLI produce).
        // Fallback: RSASSA-PKCS#1 v1.5 SHA-256 — Godot/mbedTLS clients (the
        // Victor game client) can only produce v1.5 signatures, so accepting
        // both lets every client speak over the same signed action protocol.
        // Both checks run against the same registered public key; accepting a
        // second deterministic padding of the same 2048-bit RSA-SHA256 pair
        // does not weaken the identity binding.
        let verifying_key = VerifyingKey::<Sha256>::new(pub_key.clone());
        let pss_ok = match PssSignature::try_from(signature.as_slice()) {
            Ok(sig) => verifying_key.verify(packet, &sig).is_ok(),
            Err(_) => false,
        };
        if !pss_ok {
            let v15_key = Pkcs1VerifyingKey::<Sha256>::new(pub_key);
            let v15_ok = match Pkcs1Signature::try_from(signature.as_slice()) {
                Ok(sig) => v15_key.verify(packet, &sig).is_ok(),
                Err(_) => false,
            };
            if !v15_ok {
                return (false, String::new(), false);
            }
        }

        // There is no `god::` superuser flag (ADR 0020, ADR 0036):
        // elevated authority comes from capability grants.
        let typ = creature.creature_type;
        let is_god = false;
        (true, typ, is_god)
    }

    pub(crate) fn has_access_to_store(&self, user_id: &str, store_id: &str) -> bool {
        if store_id.is_empty() {
            return false;
        }
        self.app
            .read(|trx| {
                aseman_ports::StoreAccess::is_member(
                    &crate::state::store_ports::MembershipPorts { trx },
                    store_id,
                    user_id,
                )
                .map_err(|error| anyhow::anyhow!("{error}"))
            })
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::RsaPrivateKey;
    use rsa::pkcs8::{EncodePublicKey, LineEnding};
    use rsa::pss::BlindedSigningKey;
    use rsa::rand_core::OsRng;
    use rsa::signature::{RandomizedSigner, SignatureEncoding};

    #[test]
    fn signatures_verify_against_the_creature_directory_key() {
        let node = Node::for_tests();
        let key = RsaPrivateKey::new(&mut OsRng, 1024).unwrap();
        let trx = node.tools().storage().begin(false).unwrap();
        aseman_ports::CreatureDirectory::create(
            &crate::state::creature_ports::CreaturePorts { trx: &trx },
            &aseman_domain::creature::CreatureRecord {
                id: "5@global".to_owned(),
                creature_type: "human".to_owned(),
                username: "signer@global".to_owned(),
                public_key: key
                    .to_public_key()
                    .to_public_key_pem(LineEnding::LF)
                    .unwrap(),
                chain_id: "main".to_owned(),
                subchain_id: String::new(),
                owner_id: aseman_domain::creature::HUMAN_OWNER.to_owned(),
            },
        )
        .unwrap();
        trx.commit().unwrap();

        let security = node.tools().security();
        let packet = b"{\"path\":\"/stores/signal\"}";
        let signature = B64.encode(
            BlindedSigningKey::<Sha256>::new(key)
                .sign_with_rng(&mut OsRng, packet)
                .to_vec(),
        );
        assert_eq!(
            security.auth_with_signature("5@global", packet, &signature),
            (true, "human".to_owned(), false)
        );
        assert_eq!(
            security.auth_with_signature("5@global", b"tampered", &signature),
            (false, String::new(), false)
        );
        assert_eq!(
            security.auth_with_signature("6@global", packet, &signature),
            (false, String::new(), false)
        );
    }
}
