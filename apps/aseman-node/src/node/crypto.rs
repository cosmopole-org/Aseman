//! The node's RSA keys: PKCS#8 parsing and PSS-SHA256 signatures (the salt
//! length equals the hash).

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use rsa::RsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::pss::SigningKey as PssSigningKey;
use rsa::rand_core::OsRng;
use rsa::sha2::Sha256;
use rsa::signature::{RandomizedSigner, SignatureEncoding};

use crate::node::Node;

impl Node {
    pub(crate) fn parse_private_key(pem_bytes: &[u8]) -> anyhow::Result<RsaPrivateKey> {
        let s = std::str::from_utf8(pem_bytes)?;
        Ok(RsaPrivateKey::from_pkcs8_pem(s)?)
    }

    /// Sign `data` with the given RSA key using PSS-SHA256 + the same salt
    /// length (`PSSSaltLengthEqualsHash`).
    pub(crate) fn sign_with(key: &RsaPrivateKey, data: &[u8]) -> String {
        let signing_key = PssSigningKey::<Sha256>::new(key.clone());
        let sig = signing_key.sign_with_rng(&mut OsRng, data);
        B64.encode(sig.to_bytes())
    }
}
