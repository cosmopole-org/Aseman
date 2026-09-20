//! Translation of `keygen/keygen.go` — generate an ECDSA keypair and
//! persist it under `~/.babble/{priv_key,key.pub}`.
//!
//! The Babble Go CLI used `keys.GenerateECDSAKey` (a thin wrapper around
//! `crypto/ecdsa.GenerateKey(elliptic.P256())`) and dumped the D-value as
//! plain hex. The Rust port matches that byte format using the `k256`
//! crate directly so this binary stays self-contained.

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use k256::ecdsa::SigningKey;
use k256::elliptic_curve::rand_core::OsRng;

fn main() {
    if let Err(e) = generate() {
        eprintln!("keygen failed: {}", e);
        std::process::exit(1);
    }
}

fn default_data_dir() -> PathBuf {
    let home = aseman_config::process_home_dir().unwrap_or_default();
    let suffix = match std::env::consts::OS {
        "macos" => ".Babble",
        "windows" => "AppData/Roaming/Babble",
        _ => ".babble",
    };
    PathBuf::from(home).join(suffix)
}

fn generate() -> Result<()> {
    let data_dir = default_data_dir();
    let priv_key_file = data_dir.join("priv_key");
    let pub_key_file = data_dir.join("key.pub");

    if priv_key_file.exists() {
        return Err(anyhow!("a key already lives under: {}", data_dir.display()));
    }
    fs::create_dir_all(&data_dir)?;

    let key = SigningKey::random(&mut OsRng);
    let d_hex = hex::encode(key.to_bytes());
    fs::write(&priv_key_file, d_hex.as_bytes())?;
    println!(
        "Your private key has been saved to: {}",
        priv_key_file.display()
    );

    // Public key hex: SEC1 encoded point (matches `crypto.PublicKeyHex` in
    // Babble's Go code).
    let verifying = key.verifying_key();
    let point = verifying.to_encoded_point(false);
    let pub_hex = hex::encode(point.as_bytes());
    fs::write(&pub_key_file, pub_hex.as_bytes())?;
    println!(
        "Your public key has been saved to: {}",
        pub_key_file.display()
    );
    Ok(())
}
