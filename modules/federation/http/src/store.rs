//! The federation directory and envelope guard on PostgreSQL (A704, A705).
//!
//! Descriptors are stored whole, next to the one column each comparison needs. The
//! comparison is the point: a cached descriptor is replaced only by one that moves
//! forward, so a replayed older descriptor cannot un-rotate a key.

use aseman_domain::Uuid;
use aseman_domain::federation::{Envelope, NodeDescriptor, WorkloadDescriptor};
use aseman_ports::federation::{Directory, EnvelopeGuard};
use aseman_ports::{PortError, PortResult};
use postgres::NoTls;
use r2d2::{Pool, PooledConnection};
use r2d2_postgres::PostgresConnectionManager;

type Connection = PooledConnection<PostgresConnectionManager<NoTls>>;

fn failed(error: impl std::fmt::Display) -> PortError {
    PortError::Failed(error.to_string())
}

fn db(error: postgres::Error) -> PortError {
    match error.code() {
        Some(code) if *code == postgres::error::SqlState::UNIQUE_VIOLATION => PortError::Conflict,
        _ if error.is_closed() => PortError::Unavailable("postgres"),
        _ => failed(error),
    }
}

/// This node's federation records.
pub struct PostgresFederation {
    pool: Pool<PostgresConnectionManager<NoTls>>,
    /// The node this process is. `own_node` reads its descriptor from the directory.
    node_id: Uuid,
}

impl PostgresFederation {
    #[must_use]
    pub fn new(pool: Pool<PostgresConnectionManager<NoTls>>, node_id: Uuid) -> Self {
        Self { pool, node_id }
    }

    /// Connect with an already-parsed configuration.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the pool cannot be built.
    pub fn connect_config(
        config: postgres::Config,
        max_size: u32,
        node_id: Uuid,
    ) -> PortResult<Self> {
        let pool = Pool::builder()
            .max_size(max_size.max(1))
            .build(PostgresConnectionManager::new(config, NoTls))
            .map_err(|_| PortError::Unavailable("federation database"))?;
        Ok(Self { pool, node_id })
    }

    /// Create the federation tables. Idempotent.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub fn migrate(&self) -> PortResult<()> {
        self.connection()?
            .batch_execute(crate::FEDERATION_MIGRATION)
            .map_err(db)
    }

    fn connection(&self) -> PortResult<Connection> {
        self.pool
            .get()
            .map_err(|_| PortError::Unavailable("postgres"))
    }
}

impl Directory for PostgresFederation {
    fn own_node(&self) -> PortResult<NodeDescriptor> {
        // A node with no descriptor of its own has not been enrolled; that is a
        // configuration failure, not an empty cache.
        self.node(self.node_id, i64::MIN)?
            .ok_or(PortError::NotFound)
    }

    fn node(&self, node_id: Uuid, now_millis: i64) -> PortResult<Option<NodeDescriptor>> {
        let mut connection = self.connection()?;
        let row = connection
            .query_opt(
                "SELECT descriptor, expires_at_millis FROM aseman_core.federation_node \
                 WHERE node_id = $1",
                &[&node_id],
            )
            .map_err(db)?;
        let Some(row) = row else { return Ok(None) };
        let expires: i64 = row.get("expires_at_millis");
        // An expired descriptor is not served: the home node is authoritative and this
        // is only a cache.
        if now_millis >= expires {
            return Ok(None);
        }
        let text: String = row.get("descriptor");
        serde_json::from_str(&text).map(Some).map_err(failed)
    }

    fn record_node(&self, descriptor: &NodeDescriptor) -> PortResult<()> {
        if descriptor.revoked_epochs.contains(&descriptor.key_epoch) {
            // A node cannot publish keys it has itself revoked.
            return Err(PortError::Denied(
                "the descriptor revokes its own key epoch",
            ));
        }
        let mut connection = self.connection()?;
        let sequence = i64::try_from(descriptor.sequence).map_err(failed)?;
        let text = serde_json::to_string(descriptor).map_err(failed)?;
        // The `WHERE` is the guard: a descriptor whose sequence does not move forward
        // updates no row, and the caller is told rather than silently rolled back.
        let updated = connection
            .execute(
                "INSERT INTO aseman_core.federation_node \
                   (node_id, sequence, expires_at_millis, descriptor) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (node_id) DO UPDATE SET sequence = EXCLUDED.sequence, \
                   expires_at_millis = EXCLUDED.expires_at_millis, \
                   descriptor = EXCLUDED.descriptor \
                 WHERE aseman_core.federation_node.sequence < EXCLUDED.sequence",
                &[
                    &descriptor.node_id,
                    &sequence,
                    &descriptor.expires_at_millis,
                    &text,
                ],
            )
            .map_err(db)?;
        if updated == 0 {
            return Err(PortError::Conflict);
        }
        Ok(())
    }

    fn workload(
        &self,
        workload_id: Uuid,
        now_millis: i64,
    ) -> PortResult<Option<WorkloadDescriptor>> {
        let mut connection = self.connection()?;
        let row = connection
            .query_opt(
                "SELECT descriptor, expires_at_millis FROM aseman_core.federation_workload \
                 WHERE workload_id = $1",
                &[&workload_id],
            )
            .map_err(db)?;
        let Some(row) = row else { return Ok(None) };
        let expires: i64 = row.get("expires_at_millis");
        if now_millis >= expires {
            return Ok(None);
        }
        let text: String = row.get("descriptor");
        serde_json::from_str(&text).map(Some).map_err(failed)
    }

    fn record_workload(&self, descriptor: &WorkloadDescriptor) -> PortResult<()> {
        let mut connection = self.connection()?;
        let revision = i64::try_from(descriptor.revision).map_err(failed)?;
        let text = serde_json::to_string(descriptor).map_err(failed)?;
        let updated = connection
            .execute(
                "INSERT INTO aseman_core.federation_workload \
                   (workload_id, revision, expires_at_millis, descriptor) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (workload_id) DO UPDATE SET revision = EXCLUDED.revision, \
                   expires_at_millis = EXCLUDED.expires_at_millis, \
                   descriptor = EXCLUDED.descriptor \
                 WHERE aseman_core.federation_workload.revision < EXCLUDED.revision",
                &[
                    &descriptor.workload_id,
                    &revision,
                    &descriptor.expires_at_millis,
                    &text,
                ],
            )
            .map_err(db)?;
        if updated == 0 {
            return Err(PortError::Conflict);
        }
        Ok(())
    }
}

impl EnvelopeGuard for PostgresFederation {
    fn remember_nonce(&self, envelope: &Envelope) -> PortResult<bool> {
        let mut connection = self.connection()?;
        // The primary key does the work: an insert that conflicts is a nonce this
        // node has already seen, which is a replay.
        let inserted = connection
            .execute(
                "INSERT INTO aseman_core.federation_nonce (source_node, nonce, expires_at_millis) \
                 VALUES ($1, $2, $3) ON CONFLICT (source_node, nonce) DO NOTHING",
                &[
                    &envelope.source_node,
                    &envelope.nonce,
                    &envelope.expires_at_millis,
                ],
            )
            .map_err(db)?;
        Ok(inserted == 1)
    }

    fn recorded_answer(&self, request_id: Uuid) -> PortResult<Option<String>> {
        let mut connection = self.connection()?;
        Ok(connection
            .query_opt(
                "SELECT answer FROM aseman_core.federation_answer WHERE request_id = $1",
                &[&request_id],
            )
            .map_err(db)?
            .map(|row| row.get("answer")))
    }

    fn record_answer(
        &self,
        request_id: Uuid,
        answer: &str,
        expires_at_millis: i64,
    ) -> PortResult<()> {
        let mut connection = self.connection()?;
        // The first answer stands. A second attempt to answer the same request is the
        // retry path racing itself, and must not replace what a caller may already
        // have received.
        connection
            .execute(
                "INSERT INTO aseman_core.federation_answer (request_id, answer, expires_at_millis) \
                 VALUES ($1, $2, $3) ON CONFLICT (request_id) DO NOTHING",
                &[&request_id, &answer, &expires_at_millis],
            )
            .map_err(db)?;
        Ok(())
    }

    fn purge_expired(&self, now_millis: i64) -> PortResult<u64> {
        let mut connection = self.connection()?;
        let nonces = connection
            .execute(
                "DELETE FROM aseman_core.federation_nonce WHERE expires_at_millis < $1",
                &[&now_millis],
            )
            .map_err(db)?;
        let answers = connection
            .execute(
                "DELETE FROM aseman_core.federation_answer WHERE expires_at_millis < $1",
                &[&now_millis],
            )
            .map_err(db)?;
        Ok(nonces + answers)
    }
}
