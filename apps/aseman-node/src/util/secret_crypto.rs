//! Encryption for creature-owned secrets stored on-chain.
//!
//! Creatures store secrets (API keys, tokens, …) so that the value lives in the
//! chain state only as ciphertext — a raw database/chain dump never reveals a
//! plaintext secret. Encryption uses ChaCha20-Poly1305 (AEAD) under a single
//! **node master key** that is kept OFF-chain, in the node's data directory
//! (`node-secret-key`, 0600), generated once on first use. The master key never
//! travels on-chain or over the wire, so the ciphertext on-chain is opaque
//! without it; access control (owner + revocable, time-boxed grants) is enforced
//! by the `/creatures/secret*` handlers, not here.
//!
//! Blob layout, base64 (standard) encoded: `nonce[12] || ciphertext || tag[16]`.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use anyhow::{Result, anyhow};
use aseman_fs::{Access, create_atomic};
use base64::Engine;
use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};

const MASTER_KEY_FILE: &str = "node-secret-key";
const NONCE_LEN: usize = 12;

/// The node master key in `storage_root`, created there (0600) when absent.
pub(crate) fn load_or_create_master_key(storage_root: &str) -> Result<[u8; 32]> {
    let path = Path::new(storage_root).join(MASTER_KEY_FILE);
    match read_master_key(&path)? {
        Some(key) => Ok(key),
        None => create_master_key(&path),
    }
}

/// The key stored at `path`; `None` only when there is no file. Any other read
/// failure is an error: generating a new key would orphan every secret sealed
/// under the existing one.
fn read_master_key(path: &Path) -> Result<Option<[u8; 32]>> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(anyhow!("reading node-secret-key: {error}")),
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(raw.trim())
        .map_err(|e| anyhow!("node-secret-key is not valid base64: {e}"))?;
    let key: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("node-secret-key must be 32 bytes, found {}", bytes.len()))?;
    Ok(Some(key))
}

/// Generate a key and persist it owner-only. The file is created, never
/// replaced: when another process creates it first, its key is the node's.
fn create_master_key(path: &Path) -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    getrandom::getrandom(&mut key)
        .map_err(|e| anyhow!("rng failure generating master key: {e}"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| anyhow!("creating {}: {e}", parent.display()))?;
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(key);
    match create_atomic(path, encoded.as_bytes(), Access::Private) {
        Ok(()) => Ok(key),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => read_master_key(path)?
            .ok_or_else(|| anyhow!("node-secret-key vanished while it was being created")),
        Err(error) => Err(anyhow!("installing node-secret-key: {error}")),
    }
}

/// The master key's fingerprint, stamped on every stored secret (ADR 0023): the
/// `storage migrate` writes the same one.
pub fn fingerprint(key: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"ASEMAN-LEGACY-SECRET-KEY-FINGERPRINT-V1\0");
    hasher.update(key);
    hasher.finalize().into()
}

/// Encrypt a plaintext secret under the master key. Returns the base64 blob
/// `nonce || ciphertext || tag`.
pub fn encrypt(plaintext: &[u8], key: &[u8; 32]) -> Result<String> {
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce).map_err(|e| anyhow!("rng failure generating nonce: {e}"))?;
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| anyhow!("secret encryption failed"))?;
    let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ct);
    Ok(base64::engine::general_purpose::STANDARD.encode(blob))
}

/// Decrypt a base64 blob produced by [`encrypt`] under the master key.
pub fn decrypt(blob_b64: &str, key: &[u8; 32]) -> Result<Vec<u8>> {
    let blob = base64::engine::general_purpose::STANDARD
        .decode(blob_b64.trim())
        .map_err(|e| anyhow!("secret blob is not valid base64: {e}"))?;
    if blob.len() < NONCE_LEN + 16 {
        return Err(anyhow!("secret blob too short"));
    }
    let (nonce, ct) = blob.split_at(NONCE_LEN);
    let cipher = ChaCha20Poly1305::new(Key::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| anyhow!("secret decryption failed (wrong key or corrupted blob)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "aseman-master-key-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn the_master_key_is_created_once_and_then_reused() {
        let root = scratch("reuse");
        let created = load_or_create_master_key(root.to_str().unwrap()).unwrap();
        let loaded = load_or_create_master_key(root.to_str().unwrap()).unwrap();
        assert_eq!(created, loaded);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(root.join(MASTER_KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_unreadable_master_key_is_an_error_not_a_new_key() {
        // A directory where the key belongs cannot be read as a file.
        let root = scratch("unreadable");
        fs::create_dir(root.join(MASTER_KEY_FILE)).unwrap();
        assert!(load_or_create_master_key(root.to_str().unwrap()).is_err());
        assert!(root.join(MASTER_KEY_FILE).is_dir(), "nothing was replaced");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_creator_that_loses_the_race_adopts_the_stored_key() {
        let root = scratch("race");
        let path = root.join(MASTER_KEY_FILE);
        let winner = [9u8; 32];
        fs::write(
            &path,
            base64::engine::general_purpose::STANDARD.encode(winner),
        )
        .unwrap();
        assert_eq!(create_master_key(&path).unwrap(), winner);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn round_trips() {
        let key = [7u8; 32];
        let blob = encrypt(b"sk-test-abc123", &key).unwrap();
        assert_ne!(blob, "sk-test-abc123");
        assert_eq!(decrypt(&blob, &key).unwrap(), b"sk-test-abc123");
    }

    #[test]
    fn wrong_key_fails() {
        let blob = encrypt(b"secret", &[1u8; 32]).unwrap();
        assert!(decrypt(&blob, &[2u8; 32]).is_err());
    }

    #[test]
    fn distinct_nonces() {
        let key = [3u8; 32];
        // Same plaintext encrypts to different blobs (random nonce).
        assert_ne!(encrypt(b"x", &key).unwrap(), encrypt(b"x", &key).unwrap());
    }
}
