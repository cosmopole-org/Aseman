//! RSA key pair generation and PEM decoding, and unique id strings.

use std::fs;
use std::path::Path;

use anyhow::{Result, anyhow};
use rsa::pkcs8::{DecodePublicKey, EncodePrivateKey, EncodePublicKey, LineEnding};
use rsa::rand_core::OsRng;
use rsa::{RsaPrivateKey, RsaPublicKey};
use uuid::Uuid;

/// Returns a pair of UUIDs joined by `-`. Used as request ids, packet ids,
/// pool tails, etc.
pub fn secure_unique_string() -> String {
    format!("{}-{}", Uuid::new_v4(), Uuid::new_v4())
}

/// Generates a 2048-bit RSA keypair and PEM-encodes both halves. If
/// `save_path` is non-empty the keys are also written to
/// `<save_path>/{public,private}.pem`. Returns `(private_pem, public_pem)`.
pub fn secure_key_pairs(save_path: &str) -> Result<(Vec<u8>, Vec<u8>)> {
    if !save_path.is_empty() {
        fs::create_dir_all(save_path)?;
    }

    let mut rng = OsRng;
    let private_key =
        RsaPrivateKey::new(&mut rng, 2048).map_err(|e| anyhow!("rsa generate: {}", e))?;
    let public_key = RsaPublicKey::from(&private_key);

    let priv_pem = private_key
        .to_pkcs8_pem(LineEnding::LF)
        .map_err(|e| anyhow!("encode pkcs8: {}", e))?
        .as_bytes()
        .to_vec();
    let pub_pem = public_key
        .to_public_key_pem(LineEnding::LF)
        .map_err(|e| anyhow!("encode spki: {}", e))?
        .as_bytes()
        .to_vec();

    if !save_path.is_empty() {
        let dir = Path::new(save_path);
        fs::write(dir.join("public.pem"), &pub_pem)?;
        fs::write(dir.join("private.pem"), &priv_pem)?;
    }

    Ok((priv_pem, pub_pem))
}

/// Parses a SubjectPublicKeyInfo PEM-encoded RSA public key.
pub fn parse_public_key(data: &[u8]) -> Result<RsaPublicKey> {
    let s = std::str::from_utf8(data).map_err(|e| anyhow!("utf-8: {}", e))?;
    RsaPublicKey::from_public_key_pem(s).map_err(|e| anyhow!("decode spki: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_string_is_unique() {
        let a = secure_unique_string();
        let b = secure_unique_string();
        assert_ne!(a, b);
        assert!(a.contains('-'));
    }

    #[test]
    fn unique_string_has_two_uuid_segments() {
        let s = secure_unique_string();
        // Each UUID contains 4 dashes (8-4-4-4-12). Two UUIDs joined by `-`
        // gives 4 + 1 + 4 = 9 dashes total.
        assert_eq!(s.matches('-').count(), 9);
        // The 5th dash (index 4) is the separator between the two UUIDs.
        let sep = s.match_indices('-').nth(4).expect("separator").0;
        let (a, b) = (&s[..sep], &s[sep + 1..]);
        Uuid::parse_str(a).expect("first half should be a UUID");
        Uuid::parse_str(b).expect("second half should be a UUID");
    }

    #[test]
    fn key_pairs_round_trip_through_pem_helpers() {
        let (priv_pem, pub_pem) = secure_key_pairs("").expect("generate keypair");
        assert!(
            std::str::from_utf8(&priv_pem)
                .unwrap()
                .contains("-----BEGIN PRIVATE KEY-----")
        );
        assert!(
            std::str::from_utf8(&pub_pem)
                .unwrap()
                .contains("-----BEGIN PUBLIC KEY-----")
        );

        let parsed_priv = <RsaPrivateKey as rsa::pkcs8::DecodePrivateKey>::from_pkcs8_pem(
            std::str::from_utf8(&priv_pem).unwrap(),
        )
        .expect("parse private");
        let parsed_pub = parse_public_key(&pub_pem).expect("parse public");
        // The public key derived from the parsed private must match the
        // separately-parsed public PEM.
        let derived_pub = rsa::RsaPublicKey::from(&parsed_priv);
        assert_eq!(derived_pub, parsed_pub);
    }

    #[test]
    fn key_pairs_persist_to_save_path() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("aseman-keypair-{}-{}", std::process::id(), nanos));
        let path = dir.to_string_lossy().into_owned();

        let (priv_pem, pub_pem) = secure_key_pairs(&path).expect("generate keypair");
        assert_eq!(std::fs::read(dir.join("private.pem")).unwrap(), priv_pem);
        assert_eq!(std::fs::read(dir.join("public.pem")).unwrap(), pub_pem);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_public_key_rejects_invalid_pem() {
        assert!(parse_public_key(b"not a pem").is_err());
        assert!(parse_public_key(b"").is_err());
    }
}
