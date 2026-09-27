//! Canonical node-consensus key generation shared by the Aseman binary and its
//! temporary ADR-0004 compatibility name.
#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;

use anyhow::{Result, anyhow};
use k256::ecdsa::SigningKey;
use k256::elliptic_curve::rand_core::OsRng;

fn default_data_dir() -> PathBuf {
    let home = aseman_config::process_home_dir().unwrap_or_default();
    let suffix = match std::env::consts::OS {
        "macos" => ".Babble",
        "windows" => "AppData/Roaming/Babble",
        _ => ".babble",
    };
    PathBuf::from(home).join(suffix)
}

/// Generate the legacy-compatible Babble key pair in the configured home directory.
///
/// # Errors
///
/// Refuses to overwrite an existing private key and reports filesystem failures.
pub fn generate() -> Result<()> {
    let data_dir = default_data_dir();
    let priv_key_file = data_dir.join("priv_key");
    let pub_key_file = data_dir.join("key.pub");

    if priv_key_file.exists() {
        return Err(anyhow!("a key already lives under: {}", data_dir.display()));
    }
    fs::create_dir_all(&data_dir)?;

    let key = SigningKey::random(&mut OsRng);
    fs::write(&priv_key_file, hex::encode(key.to_bytes()).as_bytes())?;
    println!(
        "Your private key has been saved to: {}",
        priv_key_file.display()
    );

    let point = key.verifying_key().to_encoded_point(false);
    fs::write(&pub_key_file, hex::encode(point.as_bytes()).as_bytes())?;
    println!(
        "Your public key has been saved to: {}",
        pub_key_file.display()
    );
    Ok(())
}
