//! The PostgreSQL provider's consensus-log storage (ADR 0035).
//!
//! Every log is a set of rows in `aseman_consensus.log_entries`, keyed by the log's
//! name and the entry key. `bytea` compares bytewise, so key order is the port's
//! order. A batch is one transaction. Setting a log aside renames its rows to
//! `<name>--UTC--<timestamp>`. In cluster mode the logs live on the home shard.

use crate::CONSENSUS_LOG_MIGRATION;
use aseman_ports::consensus_log::{ConsensusLog, ConsensusLogStorage, ConsensusLogWrite};
use aseman_ports::{PortError, PortResult};
use aseman_postgres::{Database, Pool, pool};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

fn failed(error: impl std::fmt::Display) -> PortError {
    PortError::Failed(format!("consensus log storage: {error}"))
}

/// Consensus logs in one PostgreSQL database.
pub struct PostgresConsensusLogStorage {
    pool: Pool,
}

impl PostgresConsensusLogStorage {
    /// Connect and create the log table when absent.
    pub fn connect(url: &str, max_connections: u32) -> PortResult<Self> {
        let pool = pool(&Database::parse(url).map_err(failed)?, max_connections).map_err(failed)?;
        pool.get()
            .map_err(failed)?
            .batch_execute(CONSENSUS_LOG_MIGRATION)
            .map_err(failed)?;
        Ok(Self { pool })
    }
}

impl ConsensusLogStorage for PostgresConsensusLogStorage {
    fn open(&self, name: &str, fresh: bool) -> PortResult<Arc<dyn ConsensusLog>> {
        if fresh {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(failed)?
                .as_nanos();
            self.pool
                .get()
                .map_err(failed)?
                .execute(
                    "UPDATE aseman_consensus.log_entries SET log = $2 WHERE log = $1",
                    &[&name, &format!("{name}--UTC--{now}")],
                )
                .map_err(failed)?;
        }
        Ok(Arc::new(PostgresConsensusLog {
            pool: self.pool.clone(),
            name: name.to_owned(),
        }))
    }

    fn names(&self) -> PortResult<Vec<String>> {
        Ok(self
            .pool
            .get()
            .map_err(failed)?
            .query(
                "SELECT log FROM (SELECT DISTINCT log FROM aseman_consensus.log_entries \
                 WHERE strpos(log, '--UTC--') = 0) AS logs ORDER BY log COLLATE \"C\"",
                &[],
            )
            .map_err(failed)?
            .iter()
            .map(|row| row.get(0))
            .collect())
    }
}

struct PostgresConsensusLog {
    pool: Pool,
    name: String,
}

/// The smallest byte string greater than every string starting with `prefix`, if any.
fn prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.pop() {
        if last < u8::MAX {
            end.push(last + 1);
            return Some(end);
        }
    }
    None
}

impl ConsensusLog for PostgresConsensusLog {
    fn get(&self, key: &[u8]) -> PortResult<Option<Vec<u8>>> {
        Ok(self
            .pool
            .get()
            .map_err(failed)?
            .query_opt(
                "SELECT value FROM aseman_consensus.log_entries WHERE log = $1 AND key = $2",
                &[&self.name, &key],
            )
            .map_err(failed)?
            .map(|row| row.get(0)))
    }

    fn scan_prefix(&self, prefix: &[u8]) -> PortResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut client = self.pool.get().map_err(failed)?;
        let rows = match prefix_end(prefix) {
            Some(end) => client.query(
                "SELECT key, value FROM aseman_consensus.log_entries \
                 WHERE log = $1 AND key >= $2 AND key < $3 ORDER BY key",
                &[&self.name, &prefix, &end],
            ),
            None => client.query(
                "SELECT key, value FROM aseman_consensus.log_entries \
                 WHERE log = $1 AND key >= $2 ORDER BY key",
                &[&self.name, &prefix],
            ),
        }
        .map_err(failed)?;
        Ok(rows
            .into_iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect())
    }

    fn write(&self, batch: &[ConsensusLogWrite]) -> PortResult<()> {
        let mut client = self.pool.get().map_err(failed)?;
        let mut transaction = client.transaction().map_err(failed)?;
        for write in batch {
            match write {
                ConsensusLogWrite::Put { key, value } => transaction.execute(
                    "INSERT INTO aseman_consensus.log_entries (log, key, value) VALUES ($1, $2, $3) \
                     ON CONFLICT (log, key) DO UPDATE SET value = EXCLUDED.value",
                    &[&self.name, key, value],
                ),
                ConsensusLogWrite::Delete { key } => transaction.execute(
                    "DELETE FROM aseman_consensus.log_entries WHERE log = $1 AND key = $2",
                    &[&self.name, key],
                ),
                ConsensusLogWrite::DeleteRange { start, end } => transaction.execute(
                    "DELETE FROM aseman_consensus.log_entries \
                     WHERE log = $1 AND key >= $2 AND key < $3",
                    &[&self.name, start, end],
                ),
            }
            .map_err(failed)?;
        }
        transaction.commit().map_err(failed)
    }

    fn flush(&self) -> PortResult<()> {
        // Every batch is a committed transaction; nothing is buffered.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::prefix_end;

    #[test]
    fn prefix_end_is_the_next_byte_string() {
        assert_eq!(prefix_end(b"round_"), Some(b"round`".to_vec()));
        assert_eq!(prefix_end(&[1, 255]), Some(vec![2]));
        assert_eq!(prefix_end(&[255, 255]), None);
        assert_eq!(prefix_end(b""), None);
    }
}
