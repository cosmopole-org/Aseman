//! A401 identity wire formats (`contracts/security/identity-v1.md`, ADR 0009): versioned
//! multicodec public keys, multibase text, key IDs, the canonical signed structure, and
//! the signed-request proof. The rules the verifier applies around a signature
//! (freshness, epochs, replay) live in [`aseman_domain::identity`].

use aseman_domain::identity::{
    AuthenticationError, CredentialWindow, Introduction, Proof, SignatureContext, Subject,
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature::{ED25519, Ed25519KeyPair, UnparsedPublicKey};
use rsa::RsaPublicKey;
use rsa::pkcs8::DecodePublicKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The public-key encoding version byte.
pub const KEY_ENCODING_VERSION: u8 = 1;
/// The signature protocol: the domain-separation prefix of every signed structure.
pub const SIGNATURE_PROTOCOL: &str = "aseman-signature-v1";
/// The signed-request proof version.
pub const PROOF_VERSION: u8 = 1;
/// Multibase prefix of base58btc.
const BASE58BTC: char = 'z';
/// Multihash `sha2-256` (0x12) with a 32-byte digest.
const SHA256_MULTIHASH: [u8; 2] = [0x12, 0x20];

/// A public-key algorithm and its multicodec (as an unsigned varint).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyAlgorithm {
    /// `ed25519-pub` (0xed): the only algorithm that issues canonical identities.
    Ed25519,
    /// `rsa-pub` (0x1205), SPKI DER: legacy creature keys, verification only.
    LegacyRsa,
    /// `secp256k1-pub` (0xe7), 33-byte compressed point: legacy consensus peer keys,
    /// verification owned by P8.
    LegacySecp256k1,
}

impl KeyAlgorithm {
    const fn multicodec(self) -> &'static [u8] {
        match self {
            Self::Ed25519 => &[0xed, 0x01],
            Self::LegacyRsa => &[0x85, 0x24],
            Self::LegacySecp256k1 => &[0xe7, 0x01],
        }
    }

    /// The signature algorithm name carried by proofs.
    #[must_use]
    pub const fn signature_algorithm(self) -> &'static str {
        match self {
            Self::Ed25519 => "ed25519",
            Self::LegacyRsa => "rsa-pss-sha256",
            Self::LegacySecp256k1 => "ecdsa-secp256k1-sha256",
        }
    }

    #[must_use]
    pub const fn is_legacy(self) -> bool {
        !matches!(self, Self::Ed25519)
    }
}

/// A public key in its canonical encoding:
/// `version (0x01) || multicodec varint || key bytes`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PublicKey {
    algorithm: KeyAlgorithm,
    key: Vec<u8>,
}

impl PublicKey {
    #[must_use]
    pub fn ed25519(key: [u8; 32]) -> Self {
        Self {
            algorithm: KeyAlgorithm::Ed25519,
            key: key.to_vec(),
        }
    }

    /// A legacy RSA key from its SPKI DER.
    ///
    /// # Errors
    ///
    /// `Malformed` when the DER is not an RSA SPKI key.
    pub fn legacy_rsa(spki_der: &[u8]) -> Result<Self, AuthenticationError> {
        RsaPublicKey::from_public_key_der(spki_der).map_err(|_| AuthenticationError::Malformed)?;
        Ok(Self {
            algorithm: KeyAlgorithm::LegacyRsa,
            key: spki_der.to_vec(),
        })
    }

    #[must_use]
    pub const fn algorithm(&self) -> KeyAlgorithm {
        self.algorithm
    }

    #[must_use]
    pub fn key_bytes(&self) -> &[u8] {
        &self.key
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let codec = self.algorithm.multicodec();
        let mut encoded = Vec::with_capacity(1 + codec.len() + self.key.len());
        encoded.push(KEY_ENCODING_VERSION);
        encoded.extend_from_slice(codec);
        encoded.extend_from_slice(&self.key);
        encoded
    }

    /// # Errors
    ///
    /// `UnsupportedVersion` for another encoding version, `UnsupportedAlgorithm` for an
    /// unknown multicodec, `Malformed` for a key of the wrong shape.
    pub fn decode(encoded: &[u8]) -> Result<Self, AuthenticationError> {
        let (&version, rest) = encoded
            .split_first()
            .ok_or(AuthenticationError::Malformed)?;
        if version != KEY_ENCODING_VERSION {
            return Err(AuthenticationError::UnsupportedVersion);
        }
        let (algorithm, key) = [
            KeyAlgorithm::Ed25519,
            KeyAlgorithm::LegacyRsa,
            KeyAlgorithm::LegacySecp256k1,
        ]
        .into_iter()
        .find_map(|algorithm| {
            rest.strip_prefix(algorithm.multicodec())
                .map(|key| (algorithm, key))
        })
        .ok_or(AuthenticationError::UnsupportedAlgorithm)?;
        match algorithm {
            KeyAlgorithm::Ed25519 if key.len() == 32 => Ok(Self {
                algorithm,
                key: key.to_vec(),
            }),
            KeyAlgorithm::LegacyRsa => Self::legacy_rsa(key),
            KeyAlgorithm::LegacySecp256k1 if key.len() == 33 && matches!(key[0], 2 | 3) => {
                Ok(Self {
                    algorithm,
                    key: key.to_vec(),
                })
            }
            _ => Err(AuthenticationError::Malformed),
        }
    }

    /// Multibase base58btc text of the encoding.
    #[must_use]
    pub fn to_multibase(&self) -> String {
        multibase(&self.encode())
    }

    /// # Errors
    ///
    /// As [`Self::decode`], or `Malformed` for text that is not base58btc multibase.
    pub fn from_multibase(text: &str) -> Result<Self, AuthenticationError> {
        Self::decode(&from_multibase(text)?)
    }

    /// SHA-256 of the complete versioned encoding.
    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        Sha256::digest(self.encode()).into()
    }

    /// The key ID: multibase base58btc of the `sha2-256` multihash of the fingerprint.
    #[must_use]
    pub fn key_id(&self) -> String {
        let mut multihash = SHA256_MULTIHASH.to_vec();
        multihash.extend_from_slice(&self.fingerprint());
        multibase(&multihash)
    }

    /// Verify `signature` over `message` with this key.
    ///
    /// # Errors
    ///
    /// `BadSignature`, or `UnsupportedAlgorithm` for secp256k1 (verified by P8).
    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<(), AuthenticationError> {
        match self.algorithm {
            KeyAlgorithm::Ed25519 => UnparsedPublicKey::new(&ED25519, &self.key)
                .verify(message, signature)
                .map_err(|_| AuthenticationError::BadSignature),
            KeyAlgorithm::LegacyRsa => {
                use rsa::pss::{Signature, VerifyingKey};
                use rsa::signature::Verifier;
                let key = RsaPublicKey::from_public_key_der(&self.key)
                    .map_err(|_| AuthenticationError::Malformed)?;
                let signature = Signature::try_from(signature)
                    .map_err(|_| AuthenticationError::BadSignature)?;
                VerifyingKey::<Sha256>::new(key)
                    .verify(message, &signature)
                    .map_err(|_| AuthenticationError::BadSignature)
            }
            KeyAlgorithm::LegacySecp256k1 => Err(AuthenticationError::UnsupportedAlgorithm),
        }
    }
}

fn multibase(bytes: &[u8]) -> String {
    let mut text = String::from(BASE58BTC);
    text.push_str(&bs58::encode(bytes).into_string());
    text
}

fn from_multibase(text: &str) -> Result<Vec<u8>, AuthenticationError> {
    let body = text
        .strip_prefix(BASE58BTC)
        .ok_or(AuthenticationError::Malformed)?;
    let bytes = bs58::decode(body)
        .into_vec()
        .map_err(|_| AuthenticationError::Malformed)?;
    // One spelling per value.
    if multibase(&bytes) != text {
        return Err(AuthenticationError::Malformed);
    }
    Ok(bytes)
}

/// Whether `text` is a well-formed key ID.
#[must_use]
pub fn is_key_id(text: &str) -> bool {
    from_multibase(text)
        .is_ok_and(|bytes| bytes.len() == 34 && bytes.starts_with(&SHA256_MULTIHASH))
}

/// The fields every signed structure covers, in signing order (A401 "Signed bytes").
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedFields<'a> {
    pub algorithm: &'a str,
    pub key_id: &'a str,
    pub key_epoch: u32,
    pub subject: &'a Subject,
    pub audience: &'a str,
    pub window: CredentialWindow,
    pub nonce: &'a [u8],
    pub request_id: &'a str,
    pub action: &'a str,
    pub resource: &'a str,
    pub body_digest: &'a [u8; 32],
}

/// The exact bytes a signature covers:
/// `"aseman-signature-v1" 0x00 context 0x00` followed by each field as a big-endian
/// `u32` length and its bytes, in [`SignedFields`] order. Integers are big-endian
/// (`u32` epoch, `i64` milliseconds) inside their length-prefixed fields.
#[must_use]
pub fn signing_input(context: SignatureContext, fields: &SignedFields<'_>) -> Vec<u8> {
    let mut input = Vec::with_capacity(256);
    input.extend_from_slice(SIGNATURE_PROTOCOL.as_bytes());
    input.push(0);
    input.extend_from_slice(context.as_str().as_bytes());
    input.push(0);
    let subject = fields.subject.to_string();
    let parts: [&[u8]; 13] = [
        fields.algorithm.as_bytes(),
        fields.key_id.as_bytes(),
        &fields.key_epoch.to_be_bytes(),
        subject.as_bytes(),
        fields.audience.as_bytes(),
        &fields.window.issued_at_millis.to_be_bytes(),
        &fields.window.not_before_millis.to_be_bytes(),
        &fields.window.expires_at_millis.to_be_bytes(),
        fields.nonce,
        fields.request_id.as_bytes(),
        fields.action.as_bytes(),
        fields.resource.as_bytes(),
        fields.body_digest,
    ];
    for part in parts {
        let length = u32::try_from(part.len()).unwrap_or(u32::MAX);
        input.extend_from_slice(&length.to_be_bytes());
        input.extend_from_slice(part);
    }
    input
}

/// The domain-separation prefix of an introduction record.
pub const INTRODUCTION_PROTOCOL: &str = "aseman-introduction-v1";

/// The canonical bytes of an introduction (A401 section 8):
/// `"aseman-introduction-v1" 0x00` then length-prefixed fields: subject text, key
/// encoding, `u32` epoch, `i64` not-before, and the expiry as an empty field when absent
/// or an `i64`.
#[must_use]
pub fn introduction_bytes(introduction: &Introduction) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(128);
    bytes.extend_from_slice(INTRODUCTION_PROTOCOL.as_bytes());
    bytes.push(0);
    let subject = introduction.subject.to_string();
    let expires = introduction
        .expires_at_millis
        .map(i64::to_be_bytes)
        .map(|value| value.to_vec())
        .unwrap_or_default();
    let parts: [&[u8]; 5] = [
        subject.as_bytes(),
        &introduction.public_key,
        &introduction.epoch.to_be_bytes(),
        &introduction.not_before_millis.to_be_bytes(),
        &expires,
    ];
    for part in parts {
        let length = u32::try_from(part.len()).unwrap_or(u32::MAX);
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.extend_from_slice(part);
    }
    bytes
}

/// SHA-256 of a request body, as the proof's `body_digest` carries it.
#[must_use]
pub fn body_digest(body: &[u8]) -> [u8; 32] {
    Sha256::digest(body).into()
}

/// The signed-request proof as it travels (A401 "Signed-request proof"). Byte fields
/// are unpadded base64url; times are Unix milliseconds.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRequestProof {
    pub version: u8,
    pub context: SignatureContext,
    pub algorithm: String,
    pub key_id: String,
    pub key_epoch: u32,
    pub subject: String,
    pub audience: String,
    pub issued_at_millis: i64,
    pub not_before_millis: i64,
    pub expires_at_millis: i64,
    pub nonce: String,
    pub request_id: String,
    pub action: String,
    pub resource: String,
    pub body_digest: String,
    pub signature: String,
}

fn base64url(text: &str) -> Result<Vec<u8>, AuthenticationError> {
    URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| AuthenticationError::Malformed)
}

impl SignedRequestProof {
    /// Check the encodings (A401 validation steps 1-3).
    ///
    /// # Errors
    ///
    /// `UnsupportedVersion`, `UnsupportedAlgorithm`, or `Malformed`.
    pub fn parse(&self) -> Result<Proof, AuthenticationError> {
        if self.version != PROOF_VERSION {
            return Err(AuthenticationError::UnsupportedVersion);
        }
        if !["ed25519", "rsa-pss-sha256"].contains(&self.algorithm.as_str()) {
            return Err(AuthenticationError::UnsupportedAlgorithm);
        }
        let nonce = base64url(&self.nonce)?;
        let body_digest: [u8; 32] = base64url(&self.body_digest)?
            .try_into()
            .map_err(|_| AuthenticationError::Malformed)?;
        let bounded = |text: &str| !text.is_empty() && text.len() <= 1024;
        if !(aseman_domain::identity::MIN_NONCE_BYTES..=aseman_domain::identity::MAX_NONCE_BYTES)
            .contains(&nonce.len())
            || !is_key_id(&self.key_id)
            || !bounded(&self.audience)
            || !bounded(&self.request_id)
            || !bounded(&self.action)
            || self.resource.len() > 4096
        {
            return Err(AuthenticationError::Malformed);
        }
        Ok(Proof {
            context: self.context,
            algorithm: self.algorithm.clone(),
            key_id: self.key_id.clone(),
            key_epoch: self.key_epoch,
            subject: self.subject.parse()?,
            audience: self.audience.clone(),
            window: CredentialWindow {
                issued_at_millis: self.issued_at_millis,
                not_before_millis: self.not_before_millis,
                expires_at_millis: self.expires_at_millis,
            },
            nonce,
            request_id: self.request_id.clone(),
            action: self.action.clone(),
            resource: self.resource.clone(),
            body_digest,
            signature: base64url(&self.signature)?,
        })
    }
}

/// The signed fields of a proof.
#[must_use]
pub fn proof_fields(proof: &Proof) -> SignedFields<'_> {
    SignedFields {
        algorithm: &proof.algorithm,
        key_id: &proof.key_id,
        key_epoch: proof.key_epoch,
        subject: &proof.subject,
        audience: &proof.audience,
        window: proof.window,
        nonce: &proof.nonce,
        request_id: &proof.request_id,
        action: &proof.action,
        resource: &proof.resource,
        body_digest: &proof.body_digest,
    }
}

/// Verify a proof's signature with `key` (A401 validation step 11). The key must be
/// the one `key_id` names, and its algorithm must match the proof's.
///
/// # Errors
///
/// `UnknownKey` when `key` is not the named key, `UnsupportedAlgorithm` for an
/// algorithm mismatch, or `BadSignature`.
pub fn verify_proof_signature(proof: &Proof, key: &PublicKey) -> Result<(), AuthenticationError> {
    if key.key_id() != proof.key_id {
        return Err(AuthenticationError::UnknownKey);
    }
    if key.algorithm().signature_algorithm() != proof.algorithm {
        return Err(AuthenticationError::UnsupportedAlgorithm);
    }
    key.verify(
        &signing_input(proof.context, &proof_fields(proof)),
        &proof.signature,
    )
}

/// Sign `fields` in `context` with an Ed25519 key and return the wire proof. Used by
/// nodes (tokens, descriptors), SDKs, and test vectors.
#[must_use]
pub fn sign_ed25519(
    key: &Ed25519KeyPair,
    context: SignatureContext,
    fields: &SignedFields<'_>,
) -> SignedRequestProof {
    let signature = key.sign(&signing_input(context, fields));
    SignedRequestProof {
        version: PROOF_VERSION,
        context,
        algorithm: fields.algorithm.to_owned(),
        key_id: fields.key_id.to_owned(),
        key_epoch: fields.key_epoch,
        subject: fields.subject.to_string(),
        audience: fields.audience.to_owned(),
        issued_at_millis: fields.window.issued_at_millis,
        not_before_millis: fields.window.not_before_millis,
        expires_at_millis: fields.window.expires_at_millis,
        nonce: URL_SAFE_NO_PAD.encode(fields.nonce),
        request_id: fields.request_id.to_owned(),
        action: fields.action.to_owned(),
        resource: fields.resource.to_owned(),
        body_digest: URL_SAFE_NO_PAD.encode(fields.body_digest),
        signature: URL_SAFE_NO_PAD.encode(signature.as_ref()),
    }
}

#[cfg(test)]
mod tests;
