//! Services that run outside any action (the guest API, workload control, public
//! action admission) use the storage module through [`AutoCommit`]: each call is one
//! short transaction of its own (ADR 0036).

use crate::{CapsuleStore, CapsuleStoreError, CapsuleStoreResult};
use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery};
use aseman_domain::identity::{Challenge, Subject};
use aseman_ports::{
    ChallengeStore, PortError, PortResult, PublicActionClaim, PublicActionIdempotency, ReplayGuard,
};
use aseman_storage::client::core::{identity_challenge, public_idempotency, replay_nonce};
use aseman_storage::{Mode, Models, Storage, StorageError, Trx};

pub(crate) fn port(error: StorageError) -> PortError {
    match error {
        StorageError::Conflict(_) => PortError::Conflict,
        StorageError::NotFound(_) => PortError::NotFound,
        other => PortError::Failed(other.to_string()),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// The storage module, one transaction per call.
#[derive(Clone)]
pub struct AutoCommit(pub Storage);

impl AutoCommit {
    /// Run `operation` in a transaction of its own and commit it (a read-only one is
    /// closed without writing).
    pub fn run<T>(
        &self,
        mode: Mode,
        operation: impl FnOnce(&Trx) -> Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        let trx = self.0.begin(mode)?;
        let value = operation(&trx)?;
        match mode {
            Mode::ReadWrite => trx.commit()?,
            Mode::ReadOnly => trx.rollback()?,
        }
        Ok(value)
    }

    /// Decide `operation` in a read-write transaction of its own. A commit another
    /// writer won is retried (the operation reads again and decides again); a refusal
    /// the operation itself decided comes back as its `PortError` and is not retried.
    ///
    /// # Errors
    ///
    /// The operation's refusal, a storage failure, or contention that outlasted the
    /// retries (`Conflict`).
    pub fn decide<T>(
        &self,
        operation: impl Fn(&Trx) -> Result<PortResult<T>, StorageError>,
    ) -> PortResult<T> {
        for _ in 0..crate::support::MAX_CAS_ATTEMPTS {
            let trx = self.0.begin(Mode::ReadWrite).map_err(port)?;
            let decided = match operation(&trx) {
                Ok(decided) => decided,
                Err(StorageError::Conflict(_)) => continue,
                Err(error) => return Err(port(error)),
            };
            if decided.is_err() {
                let _ = trx.rollback();
                return decided;
            }
            match trx.commit() {
                Ok(()) => return decided,
                Err(StorageError::Conflict(_)) => {}
                Err(error) => return Err(port(error)),
            }
        }
        Err(PortError::Conflict)
    }

    /// Read in a transaction of its own.
    ///
    /// # Errors
    ///
    /// A storage failure.
    pub fn read<T>(
        &self,
        operation: impl FnOnce(&Trx) -> Result<T, StorageError>,
    ) -> PortResult<T> {
        self.run(Mode::ReadOnly, operation).map_err(port)
    }

    /// The provider's clock.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable.
    pub fn now_millis(&self) -> PortResult<i64> {
        self.0.now_millis().map_err(port)
    }

    /// Remove expired nonces and challenges; how many were removed.
    pub fn purge_expired_nonces(&self, now_millis: i64) -> PortResult<u64> {
        self.run(Mode::ReadWrite, |trx| {
            let nonces = trx
                .replay_nonce()
                .delete_many(Some(replay_nonce::retain_until_millis().lte(now_millis)))?;
            let challenges = trx.identity_challenge().delete_many(Some(
                identity_challenge::expires_at_millis().lte(now_millis),
            ))?;
            Ok(nonces + challenges)
        })
        .map_err(port)
    }
}

fn store_error(error: StorageError) -> CapsuleStoreError {
    match error {
        StorageError::Conflict(_) => CapsuleStoreError::Conflict,
        other => CapsuleStoreError::Failed(other.to_string()),
    }
}

impl CapsuleStore for AutoCommit {
    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> CapsuleStoreResult<Option<CapsuleEnvelope>> {
        let trx = self.0.begin(Mode::ReadOnly).map_err(store_error)?;
        let value = trx.get(kind, id);
        let _ = trx.rollback();
        value
    }

    fn put(
        &self,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> CapsuleStoreResult<()> {
        self.put_all(&[(capsule.clone(), expected_revision)])
    }

    fn put_all(&self, writes: &[(CapsuleEnvelope, Option<u64>)]) -> CapsuleStoreResult<()> {
        let trx = self.0.begin(Mode::ReadWrite).map_err(store_error)?;
        trx.put_all(writes)?;
        trx.commit().map_err(store_error)
    }

    fn query(&self, query: &CapsuleQuery) -> CapsuleStoreResult<Vec<CapsuleEnvelope>> {
        let trx = self.0.begin(Mode::ReadOnly).map_err(store_error)?;
        let value = trx.query(query);
        let _ = trx.rollback();
        value
    }
}

impl ReplayGuard for AutoCommit {
    fn record_nonce(
        &self,
        key_id: &str,
        nonce: &[u8],
        retain_until_millis: i64,
        now_millis: i64,
    ) -> PortResult<bool> {
        let key = format!("{key_id}|{}", hex(nonce));
        let recorded = self.run(Mode::ReadWrite, |trx| {
            // A live nonce is a replay; an expired one may be used again.
            if trx
                .replay_nonce()
                .find_unique(replay_nonce::by_key(key.clone()))?
                .is_some_and(|stored| stored.retain_until_millis > now_millis)
            {
                return Ok(false);
            }
            trx.replay_nonce().upsert(
                replay_nonce::by_key(key.clone()),
                replay_nonce::Create {
                    key: key.clone(),
                    key_ref: key_id.to_owned(),
                    retain_until_millis,
                },
                replay_nonce::update().retain_until_millis(retain_until_millis),
            )?;
            Ok(true)
        });
        match recorded {
            // A concurrent first use recorded it: this one is the replay.
            Err(StorageError::Conflict(_)) => Ok(false),
            other => other.map_err(port),
        }
    }
}

impl ChallengeStore for AutoCommit {
    fn issue(
        &self,
        subject: &Subject,
        audience: &str,
        expires_at_millis: i64,
    ) -> PortResult<Challenge> {
        let nonce = [
            *uuid::Uuid::new_v4().as_bytes(),
            *uuid::Uuid::new_v4().as_bytes(),
        ]
        .concat();
        self.run(Mode::ReadWrite, |trx| {
            trx.identity_challenge().create(identity_challenge::Create {
                key: hex(&nonce),
                subject: subject.to_string(),
                audience: audience.to_owned(),
                expires_at_millis,
            })
        })
        .map_err(port)?;
        Ok(Challenge {
            nonce,
            subject: *subject,
            audience: audience.to_owned(),
            expires_at_millis,
        })
    }

    fn consume(
        &self,
        nonce: &[u8],
        subject: &Subject,
        audience: &str,
        now_millis: i64,
    ) -> PortResult<bool> {
        // Deleting the matching live challenge is the consumption; a wrong subject or
        // audience matches nothing and leaves it in place.
        let key = hex(nonce);
        let subject = subject.to_string();
        self.run(Mode::ReadWrite, |trx| {
            let Some(challenge) = trx
                .identity_challenge()
                .find_unique(identity_challenge::by_key(key.clone()))?
            else {
                return Ok(false);
            };
            if challenge.subject != subject
                || challenge.audience != audience
                || challenge.expires_at_millis <= now_millis
            {
                return Ok(false);
            }
            trx.identity_challenge()
                .delete(identity_challenge::by_key(key.clone()))?;
            Ok(true)
        })
        .map_err(port)
    }
}

fn claim_key(subject: &str, key: &str) -> String {
    format!("{subject}\u{1f}{key}")
}

impl PublicActionIdempotency for AutoCommit {
    fn claim(&self, subject: &str, key: &str, digest: [u8; 32]) -> PortResult<PublicActionClaim> {
        let record = claim_key(subject, key);
        let created = self.run(Mode::ReadWrite, |trx| {
            trx.public_idempotency().create(public_idempotency::Create {
                key: record.clone(),
                subject: subject.to_owned(),
                request_key: key.to_owned(),
                digest: digest.to_vec(),
                claimed_at_millis: now_millis(),
                response: None,
                completed: false,
            })
        });
        match created {
            Ok(_) => return Ok(PublicActionClaim::Claimed),
            Err(StorageError::Conflict(_)) => {}
            Err(error) => return Err(port(error)),
        }
        // An existing claim — in flight or completed — is classified, never re-claimed.
        let stored = self
            .run(Mode::ReadOnly, |trx| {
                trx.public_idempotency()
                    .get(public_idempotency::by_key(record.clone()))
            })
            .map_err(port)?;
        if stored.digest != digest {
            return Ok(PublicActionClaim::Mismatch);
        }
        Ok(if stored.completed {
            PublicActionClaim::Completed(stored.response.unwrap_or_default())
        } else {
            PublicActionClaim::InProgress
        })
    }

    fn complete(&self, subject: &str, key: &str, response: &[u8]) -> PortResult<()> {
        let updated = self
            .run(Mode::ReadWrite, |trx| {
                trx.public_idempotency().update(
                    public_idempotency::by_key(claim_key(subject, key)),
                    public_idempotency::update()
                        .response(Some(response.to_vec()))
                        .completed(true),
                )
            })
            .map_err(port)?;
        updated.map(drop).ok_or(PortError::NotFound)
    }

    fn release(&self, subject: &str, key: &str) -> PortResult<()> {
        self.run(Mode::ReadWrite, |trx| {
            if trx
                .public_idempotency()
                .find_unique(public_idempotency::by_key(claim_key(subject, key)))?
                .is_some_and(|claim| !claim.completed)
            {
                trx.public_idempotency()
                    .delete(public_idempotency::by_key(claim_key(subject, key)))?;
            }
            Ok(())
        })
        .map_err(port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto() -> AutoCommit {
        AutoCommit(Storage::new(
            aseman_storage::memory::MemoryProvider::new(),
            aseman_storage::schema::Schema::catalog().unwrap(),
        ))
    }

    #[test]
    fn replay_challenge_and_idempotency_semantics_hold() {
        let auto = auto();
        assert!(auto.record_nonce("k", b"n1", 100, 10).unwrap());
        assert!(!auto.record_nonce("k", b"n1", 100, 50).unwrap());
        assert!(
            auto.record_nonce("k", b"n1", 300, 150).unwrap(),
            "expired nonce reusable"
        );
        assert_eq!(auto.purge_expired_nonces(1_000).unwrap(), 1);

        let subject: Subject = "user:00000000-0000-0000-0000-000000000001".parse().unwrap();
        let challenge = auto.issue(&subject, "aud", i64::MAX).unwrap();
        assert!(
            !auto
                .consume(&challenge.nonce, &subject, "other", 0)
                .unwrap()
        );
        assert!(auto.consume(&challenge.nonce, &subject, "aud", 0).unwrap());
        assert!(!auto.consume(&challenge.nonce, &subject, "aud", 0).unwrap());

        let digest = [7; 32];
        assert_eq!(
            auto.claim("s", "k", digest).unwrap(),
            PublicActionClaim::Claimed
        );
        assert_eq!(
            auto.claim("s", "k", digest).unwrap(),
            PublicActionClaim::InProgress
        );
        assert_eq!(
            auto.claim("s", "k", [8; 32]).unwrap(),
            PublicActionClaim::Mismatch
        );
        auto.complete("s", "k", b"done").unwrap();
        assert_eq!(
            auto.claim("s", "k", digest).unwrap(),
            PublicActionClaim::Completed(b"done".to_vec())
        );
        auto.release("s", "k").unwrap();
        assert!(matches!(
            auto.claim("s", "k", digest).unwrap(),
            PublicActionClaim::Completed(_)
        ));
        assert_eq!(
            auto.claim("s", "k2", digest).unwrap(),
            PublicActionClaim::Claimed
        );
        auto.release("s", "k2").unwrap();
        assert_eq!(
            auto.claim("s", "k2", digest).unwrap(),
            PublicActionClaim::Claimed
        );
    }
}
