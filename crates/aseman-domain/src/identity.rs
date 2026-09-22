//! Identity rules (ADR 0009, A401 `contracts/security/identity-v1.md`): subjects, key
//! purposes, signature contexts, key epochs, credential freshness, and the stable
//! authentication failure codes. Pure: callers supply the clock and the stored keys.

use core::fmt;
use core::str::FromStr;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// The class of an identity. Each class has its own keys (ADR 0009).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    User,
    Creature,
    Node,
    Service,
    Workload,
    ModulePublisher,
}

impl SubjectKind {
    pub const ALL: [Self; 6] = [
        Self::User,
        Self::Creature,
        Self::Node,
        Self::Service,
        Self::Workload,
        Self::ModulePublisher,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Creature => "creature",
            Self::Node => "node",
            Self::Service => "service",
            Self::Workload => "workload",
            Self::ModulePublisher => "module_publisher",
        }
    }
}

/// A typed identity: a class and an opaque UUID, never a bare UUID (ADR 0009).
/// Its canonical text is `{kind}:{uuid}` with the UUID in lowercase hyphenated form.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct Subject {
    pub kind: SubjectKind,
    pub id: Uuid,
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind.as_str(), self.id.hyphenated())
    }
}

impl FromStr for Subject {
    type Err = AuthenticationError;

    /// Parses only the canonical text: a known kind and a lowercase hyphenated UUID.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (kind, id) = text.split_once(':').ok_or(AuthenticationError::Malformed)?;
        let kind = SubjectKind::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == kind)
            .ok_or(AuthenticationError::Malformed)?;
        let id = Uuid::parse_str(id).map_err(|_| AuthenticationError::Malformed)?;
        let subject = Self { kind, id };
        if subject.to_string() != text {
            return Err(AuthenticationError::Malformed);
        }
        Ok(subject)
    }
}

/// What a key may sign. A key has exactly one purpose.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyPurpose {
    /// Requests and challenge responses of the subject itself.
    Authentication,
    /// Node descriptors and the node's revocation statements.
    Descriptor,
    /// Capability tokens issued by a node (A403).
    TokenIssuing,
    /// Federation roots introducing other nodes.
    Introduction,
}

impl KeyPurpose {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Authentication => "authentication",
            Self::Descriptor => "descriptor",
            Self::TokenIssuing => "token_issuing",
            Self::Introduction => "introduction",
        }
    }

    /// Whether a key of this purpose may sign `context`.
    #[must_use]
    pub const fn signs(self, context: SignatureContext) -> bool {
        matches!(
            (self, context),
            (
                Self::Authentication,
                SignatureContext::Request | SignatureContext::Challenge
            ) | (Self::TokenIssuing, SignatureContext::Token)
                | (
                    Self::Descriptor,
                    SignatureContext::Descriptor | SignatureContext::Revocation
                )
                | (Self::Introduction, SignatureContext::Introduction)
        )
    }
}

/// The domain-separation context of a signature. It is part of the signed bytes, so a
/// signature for one context never verifies for another.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureContext {
    Request,
    Challenge,
    Token,
    Descriptor,
    Revocation,
    Introduction,
}

impl SignatureContext {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Challenge => "challenge",
            Self::Token => "token",
            Self::Descriptor => "descriptor",
            Self::Revocation => "revocation",
            Self::Introduction => "introduction",
        }
    }

    /// Contexts that create or extend authority. Legacy verification keys never sign
    /// them (ADR 0009: only canonical keys sign new descriptors and tokens).
    #[must_use]
    pub const fn issues_authority(self) -> bool {
        matches!(
            self,
            Self::Token | Self::Descriptor | Self::Revocation | Self::Introduction
        )
    }
}

/// Stable authentication failure codes (A401 section "Validation order"). The wire
/// renders [`AuthenticationError::code`]; the order of checks is fixed by A401.
#[derive(Clone, Copy, Debug, Eq, Error, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationError {
    #[error("unsupported signature protocol version")]
    UnsupportedVersion,
    #[error("unsupported signature algorithm")]
    UnsupportedAlgorithm,
    #[error("malformed credential")]
    Malformed,
    #[error("unknown key")]
    UnknownKey,
    #[error("the key does not belong to the subject")]
    KeySubjectMismatch,
    #[error("the key's purpose does not allow this signature")]
    KeyPurposeMismatch,
    #[error("legacy verification keys cannot sign this context")]
    LegacyKeyCannotSign,
    #[error("the key is revoked")]
    RevokedKey,
    #[error("the key epoch is not accepted")]
    EpochNotAccepted,
    #[error("the credential is for another audience")]
    AudienceMismatch,
    #[error("the credential is not valid yet")]
    NotYetValid,
    #[error("the credential has expired")]
    Expired,
    #[error("the credential lifetime exceeds the maximum")]
    LifetimeTooLong,
    #[error("the body digest does not match the body")]
    BodyDigestMismatch,
    #[error("bad signature")]
    BadSignature,
    #[error("the nonce was already used")]
    Replayed,
}

impl AuthenticationError {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedVersion => "unsupported_version",
            Self::UnsupportedAlgorithm => "unsupported_algorithm",
            Self::Malformed => "malformed",
            Self::UnknownKey => "unknown_key",
            Self::KeySubjectMismatch => "key_subject_mismatch",
            Self::KeyPurposeMismatch => "key_purpose_mismatch",
            Self::LegacyKeyCannotSign => "legacy_key_cannot_sign",
            Self::RevokedKey => "revoked_key",
            Self::EpochNotAccepted => "epoch_not_accepted",
            Self::AudienceMismatch => "audience_mismatch",
            Self::NotYetValid => "not_yet_valid",
            Self::Expired => "expired",
            Self::LifetimeTooLong => "lifetime_too_long",
            Self::BodyDigestMismatch => "body_digest_mismatch",
            Self::BadSignature => "bad_signature",
            Self::Replayed => "replayed",
        }
    }
}

/// Nonce length bounds in bytes.
pub const MIN_NONCE_BYTES: usize = 16;
pub const MAX_NONCE_BYTES: usize = 64;

/// The ADR 0009 ceiling on guest (workload) credential lifetime.
pub const MAX_GUEST_CREDENTIAL_MILLIS: i64 = 5 * 60 * 1_000;

/// Server-side time rules for one credential purpose.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FreshnessPolicy {
    /// Tolerated clock difference between signer and verifier.
    pub max_clock_skew_millis: i64,
    /// Longest accepted `expires_at - issued_at`.
    pub max_lifetime_millis: i64,
}

impl FreshnessPolicy {
    /// Guest requests: five-minute credentials, thirty seconds of skew.
    pub const GUEST: Self = Self {
        max_clock_skew_millis: 30_000,
        max_lifetime_millis: MAX_GUEST_CREDENTIAL_MILLIS,
    };
}

/// The validity window a credential claims, in Unix milliseconds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CredentialWindow {
    pub issued_at_millis: i64,
    pub not_before_millis: i64,
    pub expires_at_millis: i64,
}

impl CredentialWindow {
    /// Check the window against `now` (A401 "Freshness").
    ///
    /// # Errors
    ///
    /// `Malformed` for an inconsistent window, `LifetimeTooLong`, `NotYetValid`, or
    /// `Expired`.
    pub fn check(
        &self,
        now_millis: i64,
        policy: FreshnessPolicy,
    ) -> Result<(), AuthenticationError> {
        let skew = policy.max_clock_skew_millis;
        if self.issued_at_millis > self.not_before_millis
            || self.not_before_millis >= self.expires_at_millis
        {
            return Err(AuthenticationError::Malformed);
        }
        if self.expires_at_millis - self.issued_at_millis > policy.max_lifetime_millis {
            return Err(AuthenticationError::LifetimeTooLong);
        }
        if self.not_before_millis > now_millis.saturating_add(skew) {
            return Err(AuthenticationError::NotYetValid);
        }
        if now_millis >= self.expires_at_millis.saturating_add(skew) {
            return Err(AuthenticationError::Expired);
        }
        Ok(())
    }

    /// How long the verifier must remember the credential's nonce.
    #[must_use]
    pub fn replay_retention_until(&self, policy: FreshnessPolicy) -> i64 {
        self.expires_at_millis
            .saturating_add(policy.max_clock_skew_millis)
    }
}

/// One key epoch of a subject and purpose, as the key directory records it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeyEpoch {
    pub subject: Subject,
    pub purpose: KeyPurpose,
    /// Strictly increasing per subject and purpose, from 1. Legacy verification keys
    /// use epoch 0.
    pub epoch: u32,
    pub not_before_millis: i64,
    pub expires_at_millis: Option<i64>,
    /// When the next epoch became current; the overlap window starts here.
    pub retired_at_millis: Option<i64>,
    pub revoked_at_millis: Option<i64>,
    /// A legacy verification key (RSA or secp256k1), registered after proof of
    /// possession. It verifies but never signs authority-issuing contexts.
    pub legacy: bool,
}

/// A key as the key directory stores it (A401 section 9).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IdentityKey {
    /// The A401 key ID of `public_key`.
    pub key_id: String,
    /// The A401 versioned multicodec encoding.
    pub public_key: Vec<u8>,
    pub epoch: KeyEpoch,
}

/// What a key's encoding says about it (A401 section 2).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeyDescription {
    pub key_id: String,
    /// RSA or secp256k1: verification only, never a new canonical identity.
    pub legacy: bool,
}

/// A one-time server challenge (A401 "Replay"): its nonce answers exactly one
/// `challenge` proof by `subject` for `audience` before `expires_at_millis`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Challenge {
    pub nonce: Vec<u8>,
    pub subject: Subject,
    pub audience: String,
    pub expires_at_millis: i64,
}

/// A federation root's introduction of a node (A401 section 8): the node's descriptor
/// key for one epoch. It is the signed body of an `introduction` proof.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Introduction {
    /// The introduced node.
    pub subject: Subject,
    /// Its descriptor key, in the A401 encoding.
    pub public_key: Vec<u8>,
    pub epoch: u32,
    pub not_before_millis: i64,
    pub expires_at_millis: Option<i64>,
}

/// A signed-request proof whose encodings are valid (A401 validation steps 1-3): the
/// input to authentication.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proof {
    pub context: SignatureContext,
    pub algorithm: String,
    pub key_id: String,
    pub key_epoch: u32,
    pub subject: Subject,
    pub audience: String,
    pub window: CredentialWindow,
    pub nonce: Vec<u8>,
    pub request_id: String,
    pub action: String,
    pub resource: String,
    pub body_digest: [u8; 32],
    pub signature: Vec<u8>,
}

/// Rotation rule: the overlap during which the immediately prior epoch still verifies.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RotationPolicy {
    pub overlap_millis: i64,
}

impl RotationPolicy {
    pub const DEFAULT: Self = Self {
        overlap_millis: 24 * 60 * 60 * 1_000,
    };
}

/// Whether `epoch` of the subject's keys (all epochs of one subject and purpose) may
/// verify a signature in `context` at `now` (A401 "Epochs and rotation").
///
/// # Errors
///
/// `UnknownKey`, `RevokedKey`, `NotYetValid`, `Expired`, `EpochNotAccepted`, or
/// `LegacyKeyCannotSign`.
pub fn accept_epoch(
    epochs: &[KeyEpoch],
    epoch: u32,
    context: SignatureContext,
    now_millis: i64,
    rotation: RotationPolicy,
) -> Result<(), AuthenticationError> {
    let key = epochs
        .iter()
        .find(|candidate| candidate.epoch == epoch)
        .ok_or(AuthenticationError::UnknownKey)?;
    if !key.purpose.signs(context) {
        return Err(AuthenticationError::KeyPurposeMismatch);
    }
    // Revocation overrides every overlap.
    if key
        .revoked_at_millis
        .is_some_and(|revoked| revoked <= now_millis)
    {
        return Err(AuthenticationError::RevokedKey);
    }
    if key.legacy && context.issues_authority() {
        return Err(AuthenticationError::LegacyKeyCannotSign);
    }
    if key.not_before_millis > now_millis {
        return Err(AuthenticationError::NotYetValid);
    }
    if key
        .expires_at_millis
        .is_some_and(|expires| expires <= now_millis)
    {
        return Err(AuthenticationError::Expired);
    }
    let current = epochs
        .iter()
        .filter(|candidate| {
            candidate.not_before_millis <= now_millis
                && candidate
                    .revoked_at_millis
                    .is_none_or(|revoked| revoked > now_millis)
        })
        .map(|candidate| candidate.epoch)
        .max()
        .ok_or(AuthenticationError::EpochNotAccepted)?;
    let in_overlap = key
        .retired_at_millis
        .is_none_or(|retired| now_millis < retired.saturating_add(rotation.overlap_millis));
    if epoch == current || (epoch.saturating_add(1) == current && in_overlap) {
        Ok(())
    } else {
        Err(AuthenticationError::EpochNotAccepted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000_000;

    fn subject() -> Subject {
        Subject {
            kind: SubjectKind::Workload,
            id: Uuid::parse_str("0190f1a2-7b3c-7d4e-8f00-112233445566").unwrap(),
        }
    }

    fn key(epoch: u32) -> KeyEpoch {
        KeyEpoch {
            subject: subject(),
            purpose: KeyPurpose::Authentication,
            epoch,
            not_before_millis: NOW - 10_000,
            expires_at_millis: None,
            retired_at_millis: None,
            revoked_at_millis: None,
            legacy: false,
        }
    }

    #[test]
    fn subjects_have_one_canonical_text() {
        let text = "workload:0190f1a2-7b3c-7d4e-8f00-112233445566";
        assert_eq!(subject().to_string(), text);
        assert_eq!(text.parse::<Subject>(), Ok(subject()));
        for invalid in [
            "workload:0190F1A2-7B3C-7D4E-8F00-112233445566",
            "workload:0190f1a27b3c7d4e8f00112233445566",
            "vm:0190f1a2-7b3c-7d4e-8f00-112233445566",
            "0190f1a2-7b3c-7d4e-8f00-112233445566",
        ] {
            assert_eq!(
                invalid.parse::<Subject>(),
                Err(AuthenticationError::Malformed)
            );
        }
    }

    #[test]
    fn freshness_bounds_lifetime_skew_and_expiry() {
        let policy = FreshnessPolicy::GUEST;
        let window = |issued: i64, not_before: i64, expires: i64| CredentialWindow {
            issued_at_millis: issued,
            not_before_millis: not_before,
            expires_at_millis: expires,
        };
        assert_eq!(window(NOW, NOW, NOW + 60_000).check(NOW, policy), Ok(()));
        // Within skew on both ends.
        assert_eq!(
            window(NOW + 20_000, NOW + 20_000, NOW + 80_000).check(NOW, policy),
            Ok(())
        );
        assert_eq!(
            window(NOW - 70_000, NOW - 70_000, NOW - 10_000).check(NOW, policy),
            Ok(())
        );
        assert_eq!(
            window(NOW + 40_000, NOW + 40_000, NOW + 90_000).check(NOW, policy),
            Err(AuthenticationError::NotYetValid)
        );
        assert_eq!(
            window(NOW - 90_000, NOW - 90_000, NOW - 30_000).check(NOW, policy),
            Err(AuthenticationError::Expired)
        );
        assert_eq!(
            window(NOW, NOW, NOW + MAX_GUEST_CREDENTIAL_MILLIS + 1).check(NOW, policy),
            Err(AuthenticationError::LifetimeTooLong)
        );
        assert_eq!(
            window(NOW, NOW - 1, NOW + 1_000).check(NOW, policy),
            Err(AuthenticationError::Malformed)
        );
        assert_eq!(
            window(NOW, NOW, NOW).check(NOW, policy),
            Err(AuthenticationError::Malformed)
        );
        assert_eq!(
            window(NOW, NOW, NOW + 60_000).replay_retention_until(policy),
            NOW + 90_000
        );
    }

    #[test]
    fn rotation_accepts_current_and_prior_epoch_within_overlap_only() {
        let rotation = RotationPolicy {
            overlap_millis: 5_000,
        };
        let request = SignatureContext::Request;
        let mut prior = key(1);
        prior.retired_at_millis = Some(NOW - 1_000);
        let epochs = vec![prior.clone(), key(2)];
        assert_eq!(accept_epoch(&epochs, 2, request, NOW, rotation), Ok(()));
        assert_eq!(accept_epoch(&epochs, 1, request, NOW, rotation), Ok(()));
        assert_eq!(
            accept_epoch(&epochs, 1, request, NOW + 4_000, rotation),
            Err(AuthenticationError::EpochNotAccepted)
        );
        assert_eq!(
            accept_epoch(&epochs, 3, request, NOW, rotation),
            Err(AuthenticationError::UnknownKey)
        );
        // Only the immediately prior epoch overlaps.
        let mut older = key(0);
        older.retired_at_millis = Some(NOW - 1_000);
        let mut middle = prior.clone();
        middle.epoch = 1;
        let three = vec![older, middle, key(2)];
        assert_eq!(
            accept_epoch(&three, 0, request, NOW, rotation),
            Err(AuthenticationError::EpochNotAccepted)
        );
        // Revocation overrides the overlap.
        let mut revoked = prior;
        revoked.revoked_at_millis = Some(NOW - 1);
        assert_eq!(
            accept_epoch(&[revoked, key(2)], 1, request, NOW, rotation),
            Err(AuthenticationError::RevokedKey)
        );
        // A not-yet-valid next epoch leaves the current one current.
        let mut next = key(2);
        next.not_before_millis = NOW + 60_000;
        assert_eq!(
            accept_epoch(&[key(1), next], 1, request, NOW, rotation),
            Ok(())
        );
    }

    #[test]
    fn purposes_and_legacy_keys_limit_what_a_key_signs() {
        let rotation = RotationPolicy::DEFAULT;
        assert_eq!(
            accept_epoch(&[key(1)], 1, SignatureContext::Token, NOW, rotation),
            Err(AuthenticationError::KeyPurposeMismatch)
        );
        let mut legacy = key(0);
        legacy.legacy = true;
        assert_eq!(
            accept_epoch(
                &[legacy.clone()],
                0,
                SignatureContext::Request,
                NOW,
                rotation
            ),
            Ok(())
        );
        legacy.purpose = KeyPurpose::Descriptor;
        assert_eq!(
            accept_epoch(&[legacy], 0, SignatureContext::Descriptor, NOW, rotation),
            Err(AuthenticationError::LegacyKeyCannotSign)
        );
        assert!(KeyPurpose::Introduction.signs(SignatureContext::Introduction));
        assert!(!KeyPurpose::Introduction.signs(SignatureContext::Request));
    }
}
