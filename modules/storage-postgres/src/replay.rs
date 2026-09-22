//! The PostgreSQL [`ReplayGuard`] and [`ChallengeStore`] (A401 "Replay"): used
//! (key ID, nonce) pairs and one-time server challenges, shared by every replica of a
//! node.

use crate::PostgresCapsuleRepository;
use aseman_domain::identity::{Challenge, Subject};
use aseman_ports::{ChallengeStore, PortError, PortResult, ReplayGuard};
use ring::rand::{SecureRandom, SystemRandom};

impl PostgresCapsuleRepository {
    /// Forget nonces whose retention has ended and challenges that expired. Both stay
    /// correct without it; it only bounds the tables.
    pub fn purge_expired_nonces(&self, now_millis: i64) -> PortResult<u64> {
        self.with_client(|client| {
            let nonces = client.execute(
                "DELETE FROM aseman_core.replay_nonces WHERE retain_until_millis <= $1",
                &[&now_millis],
            )?;
            let challenges = client.execute(
                "DELETE FROM aseman_core.identity_challenges WHERE expires_at_millis <= $1",
                &[&now_millis],
            )?;
            Ok(nonces + challenges)
        })
        .map_err(|error| PortError::Failed(error.to_string()))
    }
}

impl ReplayGuard for PostgresCapsuleRepository {
    fn record_nonce(
        &self,
        key_id: &str,
        nonce: &[u8],
        retain_until_millis: i64,
        now_millis: i64,
    ) -> PortResult<bool> {
        // One statement: a new pair inserts, an expired pair is taken over, and a live
        // pair returns no row. Concurrent callers serialize on the primary key.
        self.with_client(|client| {
            client.query_opt(
                "INSERT INTO aseman_core.replay_nonces (key_id, nonce, retain_until_millis) \
                 VALUES ($1, $2, $3) \
                 ON CONFLICT (key_id, nonce) DO UPDATE \
                   SET retain_until_millis = EXCLUDED.retain_until_millis \
                   WHERE aseman_core.replay_nonces.retain_until_millis <= $4 \
                 RETURNING 1",
                &[&key_id, &nonce, &retain_until_millis, &now_millis],
            )
        })
        .map(|row| row.is_some())
        .map_err(|error| PortError::Failed(error.to_string()))
    }
}

impl ChallengeStore for PostgresCapsuleRepository {
    fn issue(
        &self,
        subject: &Subject,
        audience: &str,
        expires_at_millis: i64,
    ) -> PortResult<Challenge> {
        let mut nonce = [0u8; 32];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| PortError::Failed("no system randomness".to_owned()))?;
        let subject_text = subject.to_string();
        self.with_client(|client| {
            client.execute(
                "INSERT INTO aseman_core.identity_challenges \
                 (nonce, subject, audience, expires_at_millis) VALUES ($1, $2, $3, $4)",
                &[
                    &nonce.as_slice(),
                    &subject_text,
                    &audience,
                    &expires_at_millis,
                ],
            )
        })
        .map_err(|error| PortError::Failed(error.to_string()))?;
        Ok(Challenge {
            nonce: nonce.to_vec(),
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
        // Deleting the matching live row is the consumption; a wrong subject or
        // audience matches nothing and leaves the challenge in place.
        let subject_text = subject.to_string();
        self.with_client(|client| {
            client.execute(
                "DELETE FROM aseman_core.identity_challenges \
                 WHERE nonce = $1 AND subject = $2 AND audience = $3 \
                   AND expires_at_millis > $4",
                &[&nonce, &subject_text, &audience, &now_millis],
            )
        })
        .map(|deleted| deleted == 1)
        .map_err(|error| PortError::Failed(error.to_string()))
    }
}
