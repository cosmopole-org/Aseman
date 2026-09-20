//! Translation of `chain/crypto/keys/key_reader_writer.go`.

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use anyhow::Result;
use k256::ecdsa::SigningKey;

use super::private_key::{dump_private_key, parse_private_key};

/// Reads and writes ECDSA keys from/to any format or support.
///
/// The Go interface signature was inconsistent with `SimpleKeyfile`'s actual
/// methods; this trait matches the working implementation.
pub trait KeyReaderWriter {
    fn read_key(&self) -> Result<SigningKey>;
    fn write_key(&self, key: &SigningKey) -> Result<()>;
}

/// Implements [`KeyReaderWriter`] with unencrypted and unformatted files.
pub struct SimpleKeyfile {
    l: Mutex<()>,
    keyfile: String,
}

impl SimpleKeyfile {
    /// Instantiates a new `SimpleKeyfile` with an underlying file.
    pub fn new(keyfile: &str) -> SimpleKeyfile {
        SimpleKeyfile {
            l: Mutex::new(()),
            keyfile: keyfile.to_string(),
        }
    }
}

impl KeyReaderWriter for SimpleKeyfile {
    /// Reads a raw hex dump of the key's D value from the underlying file.
    fn read_key(&self) -> Result<SigningKey> {
        let _guard = self.l.lock().unwrap();
        let buf = fs::read_to_string(&self.keyfile)?;
        let trimmed = buf.trim();
        let key = hex::decode(trimmed)?;
        parse_private_key(&key)
    }

    /// Writes a raw hex dump of the key's D value to the underlying file.
    fn write_key(&self, key: &SigningKey) -> Result<()> {
        let _guard = self.l.lock().unwrap();
        let raw_key = hex::encode(dump_private_key(key));

        if let Some(parent) = Path::new(&self.keyfile).parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        fs::write(&self.keyfile, raw_key)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::network::chain::crypto::keys::private_key::{
        dump_private_key, generate_ecdsa_key,
    };

    fn temp_dir() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("babble-keys-{}-{}", std::process::id(), nanos));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Translation of keys_test.go::TestSimpleKeyfile.
    #[test]
    fn test_simple_keyfile() {
        let dir = temp_dir();
        let simple_keyfile = SimpleKeyfile::new(dir.join("priv_key").to_str().unwrap());

        // Try a read, should get an error (no file yet).
        assert!(
            simple_keyfile.read_key().is_err(),
            "ReadKey should generate an error"
        );

        // Initialize a key and try a write.
        let key = generate_ecdsa_key().unwrap();
        simple_keyfile.write_key(&key).expect("write key");

        // Try a read, should get the key back.
        let n_key = simple_keyfile.read_key().expect("read key");
        assert_eq!(
            dump_private_key(&n_key),
            dump_private_key(&key),
            "Keys do not match"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_key_creates_intermediate_directories() {
        let dir = temp_dir();
        // Two levels of nested directories that don't exist yet.
        let nested = dir.join("a").join("b").join("priv_key");
        let keyfile = SimpleKeyfile::new(nested.to_str().unwrap());
        let key = generate_ecdsa_key().unwrap();
        keyfile.write_key(&key).expect("write should mkdir -p");
        assert!(nested.exists(), "keyfile must exist on disk");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_key_rejects_non_hex_contents() {
        let dir = temp_dir();
        let path = dir.join("bogus_priv_key");
        std::fs::write(&path, b"not hex content at all").unwrap();
        let keyfile = SimpleKeyfile::new(path.to_str().unwrap());
        assert!(keyfile.read_key().is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_key_rejects_hex_of_wrong_length() {
        let dir = temp_dir();
        let path = dir.join("short_priv_key");
        // 16 hex chars = 8 bytes, but a secp256k1 D value needs 32 bytes.
        std::fs::write(&path, b"abcdef0123456789").unwrap();
        let keyfile = SimpleKeyfile::new(path.to_str().unwrap());
        assert!(keyfile.read_key().is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_then_read_round_trip_preserves_d_value_after_trimming() {
        // Files written by `write_key` contain no trailing newline, but
        // `read_key` should tolerate one if a user appended one manually.
        let dir = temp_dir();
        let path = dir.join("trim_priv_key");
        let key = generate_ecdsa_key().unwrap();
        let raw = format!("{}\n  \n", hex::encode(dump_private_key(&key)));
        std::fs::write(&path, raw).unwrap();
        let keyfile = SimpleKeyfile::new(path.to_str().unwrap());
        let loaded = keyfile.read_key().expect("read should trim");
        assert_eq!(dump_private_key(&loaded), dump_private_key(&key));
        let _ = fs::remove_dir_all(&dir);
    }
}
