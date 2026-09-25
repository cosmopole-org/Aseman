//! The durable public action idempotency store (A701, P7-06): one claim per
//! (subject, key) in `aseman_core.public_idempotency`. The digest is what the first
//! request signed; a retry under the same key replays the recorded response, a
//! different digest is a mismatch, and an unfinished claim is in progress. Claims are
//! node state, so they share the node's core schema.

use crate::PostgresCapsuleRepository;
use aseman_ports::{PortError, PortResult, PublicActionClaim, PublicActionIdempotency};

/// The node's clock is authoritative for claim time, so retries compare against the
/// same clock the store uses.
const NOW_MILLIS: &str = "(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint";

impl PublicActionIdempotency for PostgresCapsuleRepository {
    fn claim(&self, subject: &str, key: &str, digest: [u8; 32]) -> PortResult<PublicActionClaim> {
        let claimed = self
            .with_client(|client| {
                // One statement: only a missing row inserts (a new claim). An existing
                // row — in flight or completed — is left untouched and classified
                // below, so a live in-flight claim is never re-claimed.
                client.query_opt(
                    &format!(
                        "INSERT INTO aseman_core.public_idempotency \
                            (subject, key, digest, claimed_at_millis) VALUES ($1, $2, $3, {NOW_MILLIS}) \
                            ON CONFLICT (subject, key) DO NOTHING \
                            RETURNING 1"
                    ),
                    &[&subject, &key, &digest.as_slice()],
                )
            })
            .map_err(|error| PortError::Failed(error.to_string()))?
            .is_some();
        if claimed {
            return Ok(PublicActionClaim::Claimed);
        }
        let row = self
            .with_client(|client| {
                client.query_one(
                    "SELECT digest, completed, response FROM aseman_core.public_idempotency \
                     WHERE subject = $1 AND key = $2",
                    &[&subject, &key],
                )
            })
            .map_err(|error| PortError::Failed(error.to_string()))?;
        let stored: Vec<u8> = row.get(0);
        if stored != digest {
            return Ok(PublicActionClaim::Mismatch);
        }
        let completed: bool = row.get(1);
        Ok(if completed {
            PublicActionClaim::Completed(row.get::<_, Option<Vec<u8>>>(2).unwrap_or_default())
        } else {
            PublicActionClaim::InProgress
        })
    }

    fn complete(&self, subject: &str, key: &str, response: &[u8]) -> PortResult<()> {
        self.with_client(|client| {
            client.execute(
                "UPDATE aseman_core.public_idempotency \
                 SET response = $3, completed = TRUE \
                 WHERE subject = $1 AND key = $2",
                &[&subject, &key, &response],
            )
        })
        .map(|updated| {
            if updated == 1 {
                Ok(())
            } else {
                Err(PortError::NotFound)
            }
        })
        .map_err(|error| PortError::Failed(error.to_string()))?
    }

    fn release(&self, subject: &str, key: &str) -> PortResult<()> {
        self.with_client(|client| {
            client.execute(
                "DELETE FROM aseman_core.public_idempotency \
                 WHERE subject = $1 AND key = $2 AND completed IS FALSE",
                &[&subject, &key],
            )
        })
        .map(|_| ())
        .map_err(|error| PortError::Failed(error.to_string()))
    }
}
