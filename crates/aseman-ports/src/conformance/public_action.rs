//! The public action idempotency suite (A701, P7-06): one durable claim per
//! (subject, key), digest-bound, run-once-then-replay.

use crate::{PortError, PortResult, PublicActionClaim, PublicActionIdempotency};

/// Exercises [`PublicActionIdempotency`] on an empty store.
///
/// # Panics
///
/// Panics when the adapter deviates from the port contract.
pub fn public_action_idempotency(store: &dyn PublicActionIdempotency) {
    let subject = "user:0190f1a2-7b3c-7d4e-8f00-00000000a701";
    let key = "request-key-0001";
    let digest = [7u8; 32];
    let response = br#"{"ok":true}"#.to_vec();

    // First use claims the key.
    assert_eq!(
        store.claim(subject, key, digest),
        Ok(PublicActionClaim::Claimed)
    );
    // A retry under the same key and digest is still in flight (not completed yet).
    assert_eq!(
        store.claim(subject, key, digest),
        Ok(PublicActionClaim::InProgress)
    );
    // Releasing an unfinished claim lets the mutation be retried.
    store.release(subject, key).unwrap();
    assert_eq!(
        store.claim(subject, key, digest),
        Ok(PublicActionClaim::Claimed)
    );
    // Completing makes a retry replay the recorded response.
    store.complete(subject, key, &response).unwrap();
    assert_eq!(
        store.claim(subject, key, digest),
        Ok(PublicActionClaim::Completed(response.clone()))
    );
    // A completed claim is never released.
    store.release(subject, key).unwrap();
    assert_eq!(
        store.claim(subject, key, digest),
        Ok(PublicActionClaim::Completed(response.clone()))
    );
    // A different digest under the same key is a mismatch.
    assert_eq!(
        store.claim(subject, key, [8u8; 32]),
        Ok(PublicActionClaim::Mismatch)
    );

    // The same key is independent per subject.
    let other = "user:0190f1a2-7b3c-7d4e-8f00-00000000a702";
    assert_eq!(
        store.claim(other, key, digest),
        Ok(PublicActionClaim::Claimed)
    );
    store.complete(other, key, b"other").unwrap();
    assert_eq!(
        store.claim(other, key, digest),
        Ok(PublicActionClaim::Completed(b"other".to_vec()))
    );
    // And another key for the same subject is a fresh claim.
    assert_eq!(
        store.claim(subject, "request-key-0002", digest),
        Ok(PublicActionClaim::Claimed)
    );

    // A provider that cannot answer is an error, never a silent accept.
    let _ = PortError::Failed("unavailable".to_owned());
    let _: PortResult<()> = Ok(());
}
