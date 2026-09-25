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
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Result, anyhow};
use base64::Engine;
use chacha20poly1305::aead::Aead;
use chacha20poly1305::{ChaCha20Poly1305, Key, KeyInit, Nonce};

const MASTER_KEY_FILE: &str = "node-secret-key";
const NONCE_LEN: usize = 12;

static MASTER_KEY: OnceLock<[u8; 32]> = OnceLock::new();

/// The node master key, loaded from `<storage_root>/node-secret-key` or created
/// there (0600) on first use. Cached for the process; the first caller's
/// `storage_root` wins (it is stable for a running node).
pub fn master_key(storage_root: &str) -> Result<[u8; 32]> {
    if let Some(k) = MASTER_KEY.get() {
        return Ok(*k);
    }
    let key = load_or_create_master_key(storage_root)?;
    // If another thread raced us, keep whichever landed first — both read the
    // same file, so the bytes are identical anyway.
    let _ = MASTER_KEY.set(key);
    Ok(*MASTER_KEY.get().unwrap())
}

fn load_or_create_master_key(storage_root: &str) -> Result<[u8; 32]> {
    let path = Path::new(storage_root).join(MASTER_KEY_FILE);
    if let Ok(raw) = fs::read_to_string(&path) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(raw.trim())
            .map_err(|e| anyhow!("node-secret-key is not valid base64: {e}"))?;
        if bytes.len() != 32 {
            return Err(anyhow!(
                "node-secret-key must be 32 bytes, found {}",
                bytes.len()
            ));
        }
        let mut k = [0u8; 32];
        k.copy_from_slice(&bytes);
        return Ok(k);
    }
    // Generate a fresh key and persist it 0600 so it survives restarts. Written
    // via a temp file + rename so a crash mid-write cannot leave a truncated key.
    let mut k = [0u8; 32];
    getrandom::getrandom(&mut k).map_err(|e| anyhow!("rng failure generating master key: {e}"))?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(k);
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, &encoded).map_err(|e| anyhow!("writing node-secret-key: {e}"))?;
    set_owner_only(&tmp);
    fs::rename(&tmp, &path).map_err(|e| anyhow!("installing node-secret-key: {e}"))?;
    set_owner_only(&path);
    Ok(k)
}

#[cfg(unix)]
fn set_owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) {}

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
