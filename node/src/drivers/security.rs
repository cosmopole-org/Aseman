//! Translation of `drivers/security/security.go`.
//!
//! `Security` implements [`ISecurity`] for the Caspar node: per-tag RSA
//! keypair management (loaded from `<storage_root>/keys/<tag>/`), PKCS#1 v1.5
//! encrypt/decrypt, and PSS-SHA256 signature verification against a public
//! key fetched from the `ITrx`-backed key store.

use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex};

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rsa::pkcs1v15::Pkcs1v15Encrypt;
use rsa::pkcs1v15::{Signature as Pkcs1Signature, VerifyingKey as Pkcs1VerifyingKey};
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::pss::{Signature as PssSignature, VerifyingKey};
use rsa::rand_core::OsRng;
use rsa::sha2::Sha256;
use rsa::signature::Verifier;
use rsa::{RsaPrivateKey, RsaPublicKey};

use crate::models::core::ICore;
use crate::models::ports::security::ISecurity;
use crate::models::ports::signaler::ISignaler;
use crate::models::ports::storage::IStorage;
use crate::models::transaction::ITrx;
use crate::shell::utils::crypto as cryp;

const KEYS_FOLDER: &str = "keys";

/// Concrete [`ISecurity`] implementation.
pub struct Security {
    app: Arc<dyn ICore>,
    _storage: Arc<dyn IStorage>,
    _signaler: Arc<dyn ISignaler>,
    storage_root: String,
    keys: Mutex<HashMap<String, Vec<Vec<u8>>>>,
}

impl Security {
    /// `New(core, storageRoot, storage, signaler)`.
    pub fn new(
        app: Arc<dyn ICore>,
        storage_root: &str,
        storage: Arc<dyn IStorage>,
        signaler: Arc<dyn ISignaler>,
    ) -> Arc<Security> {
        let s = Arc::new(Security {
            app,
            _storage: storage,
            _signaler: signaler,
            storage_root: storage_root.to_string(),
            keys: Mutex::new(HashMap::new()),
        });
        s.load_keys();
        s
    }
}

impl ISecurity for Security {
    fn load_keys(&self) {
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

    fn generate_secure_key_pair(&self, tag: &str) {
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

    fn fetch_key_pair(&self, tag: &str) -> Vec<Vec<u8>> {
        self.keys
            .lock()
            .unwrap()
            .get(tag)
            .cloned()
            .unwrap_or_default()
    }

    fn encrypt(&self, tag: &str, plain_text: &str) -> String {
        let pair = self.fetch_key_pair(tag);
        if pair.len() < 2 {
            return String::new();
        }
        let pub_pem = match std::str::from_utf8(&pair[1]) {
            Ok(s) => s,
            Err(_) => return String::new(),
        };
        let pk = match RsaPublicKey::from_public_key_pem(pub_pem) {
            Ok(k) => k,
            Err(_) => return String::new(),
        };
        let cipher = match pk.encrypt(&mut OsRng, Pkcs1v15Encrypt, plain_text.as_bytes()) {
            Ok(c) => c,
            Err(_) => return String::new(),
        };
        hex::encode(cipher)
    }

    fn decrypt(&self, tag: &str, cipher_text: &str) -> String {
        let pair = self.fetch_key_pair(tag);
        if pair.is_empty() {
            return String::new();
        }
        let raw = match hex::decode(cipher_text) {
            Ok(b) => b,
            Err(_) => return String::new(),
        };
        let priv_pem = match std::str::from_utf8(&pair[0]) {
            Ok(s) => s,
            Err(_) => return String::new(),
        };
        let sk = match RsaPrivateKey::from_pkcs8_pem(priv_pem) {
            Ok(k) => k,
            Err(_) => return String::new(),
        };
        match sk.decrypt(Pkcs1v15Encrypt, &raw) {
            Ok(plain) => String::from_utf8_lossy(&plain).into_owned(),
            Err(_) => String::new(),
        }
    }

    fn auth_with_signature(
        &self,
        user_id: &str,
        packet: &[u8],
        signature_base64: &str,
    ) -> (bool, String, bool) {
        // Look up the user's public key.
        let pub_key_slot: Arc<Mutex<Option<RsaPublicKey>>> = Arc::new(Mutex::new(None));
        let slot_clone = pub_key_slot.clone();
        let user_id_owned = user_id.to_string();
        self.app.modify_state(
            true,
            Box::new(move |trx: &dyn ITrx| {
                *slot_clone.lock().unwrap() = trx.get_pub_key(&user_id_owned);
                Ok(())
            }),
        );
        let pub_key = match pub_key_slot.lock().unwrap().clone() {
            Some(k) => k,
            None => return (false, String::new(), false),
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

        // Successful — fetch user type / god flag.
        let typ_slot = Arc::new(Mutex::new(String::new()));
        let god_slot = Arc::new(Mutex::new(false));
        let typ_clone = typ_slot.clone();
        let god_clone = god_slot.clone();
        let user_id_owned = user_id.to_string();
        self.app.modify_state(
            true,
            Box::new(move |trx: &dyn ITrx| {
                *typ_clone.lock().unwrap() = aseman_ports::CreatureDirectory::creature(
                    &crate::shell::api::model::creature_ports::LegacyCreatures { trx },
                    &user_id_owned,
                )
                .ok()
                .flatten()
                .map(|record| record.creature_type)
                .unwrap_or_default();
                *god_clone.lock().unwrap() =
                    trx.get_string(&format!("god::{}", user_id_owned)) == "true";
                Ok(())
            }),
        );
        let typ = typ_slot.lock().unwrap().clone();
        let is_god = *god_slot.lock().unwrap();
        (true, typ, is_god)
    }

    fn has_access_to_store(&self, user_id: &str, store_id: &str) -> bool {
        if store_id.is_empty() {
            return false;
        }
        let found = Arc::new(Mutex::new(false));
        let found_clone = found.clone();
        let (user_id, store_id) = (user_id.to_string(), store_id.to_string());
        self.app.modify_state(
            true,
            Box::new(move |trx: &dyn ITrx| {
                // Membership goes through the store port (legacy adapter until cutover).
                let ports = crate::shell::api::model::store_ports::LegacyMembership { trx };
                let member = aseman_ports::StoreAccess::is_member(&ports, &store_id, &user_id)
                    .unwrap_or(false);
                *found_clone.lock().unwrap() = member;
                Ok(())
            }),
        );
        let res = *found.lock().unwrap();
        res
    }
}
