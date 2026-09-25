//! Fenced singleton leases on PostgreSQL (A607, ADR 0013).
//!
//! Every decision is made inside one transaction that locks the lease row, against
//! database time. The caller's clock is never consulted: a replica whose clock has
//! drifted, or whose process was paused, must not be able to convince itself it still
//! holds a lease.

use aseman_domain::coordination::{Acquisition, FencingToken, Lease, LeaseName, plan_acquisition};
use aseman_ports::coordination::{CoordinationPort, FencedDestination};
use aseman_ports::{PortError, PortResult};
use postgres::NoTls;
use r2d2::{Pool, PooledConnection};
use r2d2_postgres::PostgresConnectionManager;

type Connection = PooledConnection<PostgresConnectionManager<NoTls>>;

/// Database time in milliseconds. Postgres gives microseconds since the epoch from
/// `clock_timestamp()`, which — unlike `now()` — advances inside a transaction.
const NOW_MILLIS: &str = "(EXTRACT(EPOCH FROM clock_timestamp()) * 1000)::bigint";

fn failed(error: impl std::fmt::Display) -> PortError {
    PortError::Failed(error.to_string())
}

fn db(error: postgres::Error) -> PortError {
    if error.is_closed() {
        PortError::Unavailable("postgres")
    } else {
        failed(error)
    }
}

/// The coordination port on PostgreSQL.
pub struct PostgresCoordination {
    pool: Pool<PostgresConnectionManager<NoTls>>,
}

impl PostgresCoordination {
    #[must_use]
    pub fn new(pool: Pool<PostgresConnectionManager<NoTls>>) -> Self {
        Self { pool }
    }

    /// Connect to `url` with a pool of at most `max_size` connections.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the URL is invalid or the pool cannot be built.
    pub fn connect(url: &str, max_size: u32) -> PortResult<Self> {
        let config = url
            .parse::<postgres::Config>()
            .map_err(|_| PortError::Unavailable("invalid coordination database URL"))?;
        Self::connect_config(config, max_size)
    }

    /// Connect with a pool of at most `max_size` connections.
    ///
    /// # Errors
    ///
    /// `Unavailable` when the configuration cannot be used or the pool cannot be
    /// built.
    pub fn connect_config(config: postgres::Config, max_size: u32) -> PortResult<Self> {
        let pool = Pool::builder()
            .max_size(max_size.max(1))
            .build(PostgresConnectionManager::new(config, NoTls))
            .map_err(|_| PortError::Unavailable("coordination database"))?;
        Ok(Self { pool })
    }

    /// Create the coordination tables. Idempotent, and safe to run from either the
    /// node or the VMM service: they share a database and start in either order.
    ///
    /// # Errors
    ///
    /// Database failures.
    pub fn migrate(&self) -> PortResult<()> {
        self.connection()?
            .batch_execute(crate::COORDINATION_MIGRATION)
            .map_err(db)
    }

    fn connection(&self) -> PortResult<Connection> {
        self.pool
            .get()
            .map_err(|_| PortError::Unavailable("postgres"))
    }

    /// The provider's time, inside the caller's transaction.
    fn now(transaction: &mut postgres::Transaction<'_>) -> PortResult<i64> {
        Ok(transaction
            .query_one(&format!("SELECT {NOW_MILLIS} AS now"), &[])
            .map_err(db)?
            .get("now"))
    }

    /// The lease row, as a domain value.
    fn row_to_lease(name: &LeaseName, row: &postgres::Row) -> PortResult<Lease> {
        let token: i64 = row.get("token");
        Ok(Lease {
            name: name.clone(),
            instance: row.get("instance"),
            token: FencingToken::from_stored(u64::try_from(token).map_err(failed)?)
                .map_err(failed)?,
            acquired_at_millis: row.get("acquired_at_millis"),
            expires_at_millis: row.get("expires_at_millis"),
        })
    }
}

impl CoordinationPort for PostgresCoordination {
    fn acquire(
        &self,
        name: &LeaseName,
        instance: &str,
        ttl_millis: i64,
    ) -> PortResult<Acquisition> {
        let mut connection = self.connection()?;
        let mut transaction = connection.transaction().map_err(db)?;
        let now = Self::now(&mut transaction)?;

        // `SELECT ... FOR UPDATE` locks nothing when the row does not exist, so two
        // replicas racing for a brand new lease would both see "free" and both be
        // granted token 1. The insert below is the race: the primary key serializes
        // it, exactly one caller inserts, and everyone else falls through to the
        // lock. The values are the ones the domain decided for a free lease, so the
        // rule lives in one place.
        let Acquisition::Granted(fresh) =
            plan_acquisition(name, None, instance, now, ttl_millis).map_err(failed)?
        else {
            unreachable!("a free lease is always granted");
        };
        let inserted = transaction
            .execute(
                "INSERT INTO aseman_core.coordination_lease \
                   (name, instance, token, acquired_at_millis, expires_at_millis) \
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (name) DO NOTHING",
                &[
                    &name.as_str(),
                    &fresh.instance,
                    &i64::try_from(fresh.token.get()).map_err(failed)?,
                    &fresh.acquired_at_millis,
                    &fresh.expires_at_millis,
                ],
            )
            .map_err(db)?;
        if inserted == 1 {
            transaction.commit().map_err(db)?;
            return Ok(Acquisition::Granted(fresh));
        }

        // The row exists, so the lock has something to take. A concurrent acquirer
        // waits here and then reads what this one wrote.
        let current = transaction
            .query_one(
                "SELECT instance, token, acquired_at_millis, expires_at_millis \
                 FROM aseman_core.coordination_lease WHERE name = $1 FOR UPDATE",
                &[&name.as_str()],
            )
            .map_err(db)
            .and_then(|row| Self::row_to_lease(name, &row))?;
        // Time is read again: waiting for the lock can take as long as the lease.
        let now = Self::now(&mut transaction)?;
        let planned =
            plan_acquisition(name, Some(&current), instance, now, ttl_millis).map_err(failed)?;
        let Acquisition::Granted(lease) = &planned else {
            // Nothing is written: the holder keeps its row and its expiry.
            transaction.commit().map_err(db)?;
            return Ok(planned);
        };
        transaction
            .execute(
                "UPDATE aseman_core.coordination_lease \
                 SET instance = $2, token = $3, acquired_at_millis = $4, \
                     expires_at_millis = $5 \
                 WHERE name = $1",
                &[
                    &name.as_str(),
                    &lease.instance,
                    &i64::try_from(lease.token.get()).map_err(failed)?,
                    &lease.acquired_at_millis,
                    &lease.expires_at_millis,
                ],
            )
            .map_err(db)?;
        transaction.commit().map_err(db)?;
        Ok(planned)
    }

    fn renew(&self, lease: &Lease, ttl_millis: i64) -> PortResult<Option<Lease>> {
        if ttl_millis <= 0 {
            return Err(PortError::Denied("a lease time to live is positive"));
        }
        let mut connection = self.connection()?;
        let token = i64::try_from(lease.token.get()).map_err(failed)?;
        // The owner, the token, and an unexpired row are all required: a holder that
        // has been taken over gets `None` and must stop, not retry.
        let row = connection
            .query_opt(
                &format!(
                    "UPDATE aseman_core.coordination_lease \
                     SET expires_at_millis = {NOW_MILLIS} + $1 \
                     WHERE name = $2 AND instance = $3 AND token = $4 \
                       AND expires_at_millis > {NOW_MILLIS} \
                     RETURNING instance, token, acquired_at_millis, expires_at_millis"
                ),
                &[&ttl_millis, &lease.name.as_str(), &lease.instance, &token],
            )
            .map_err(db)?;
        row.map(|row| Self::row_to_lease(&lease.name, &row))
            .transpose()
    }

    fn release(&self, lease: &Lease) -> PortResult<()> {
        let mut connection = self.connection()?;
        let token = i64::try_from(lease.token.get()).map_err(failed)?;
        // A release expires the lease; it never deletes the row. The row carries the
        // token counter, and tokens must keep rising for the life of the name — a
        // deleted row would hand the next holder token 1 again and unfence every
        // effect committed before it.
        //
        // Scoped to this holder and token: a stale release must never free the lease
        // the next holder has already taken.
        connection
            .execute(
                &format!(
                    "UPDATE aseman_core.coordination_lease \
                     SET expires_at_millis = GREATEST(acquired_at_millis, {NOW_MILLIS}) \
                     WHERE name = $1 AND instance = $2 AND token = $3"
                ),
                &[&lease.name.as_str(), &lease.instance, &token],
            )
            .map_err(db)?;
        Ok(())
    }

    fn read(&self, name: &LeaseName) -> PortResult<Option<Lease>> {
        let mut connection = self.connection()?;
        connection
            .query_opt(
                "SELECT instance, token, acquired_at_millis, expires_at_millis \
                 FROM aseman_core.coordination_lease WHERE name = $1",
                &[&name.as_str()],
            )
            .map_err(db)?
            .map(|row| Self::row_to_lease(name, &row))
            .transpose()
    }

    fn now_millis(&self) -> PortResult<i64> {
        let mut connection = self.connection()?;
        Ok(connection
            .query_one(&format!("SELECT {NOW_MILLIS} AS now"), &[])
            .map_err(db)?
            .get("now"))
    }
}

impl FencedDestination for PostgresCoordination {
    fn last_accepted(&self, name: &LeaseName) -> PortResult<Option<FencingToken>> {
        let mut connection = self.connection()?;
        connection
            .query_opt(
                "SELECT token FROM aseman_core.coordination_fence WHERE name = $1",
                &[&name.as_str()],
            )
            .map_err(db)?
            .map(|row| {
                let token: i64 = row.get("token");
                FencingToken::from_stored(u64::try_from(token).map_err(failed)?).map_err(failed)
            })
            .transpose()
    }

    fn accept(&self, name: &LeaseName, token: FencingToken) -> PortResult<()> {
        let mut connection = self.connection()?;
        let token = i64::try_from(token.get()).map_err(failed)?;
        // The `WHERE` clause is the guard: an insert that would move the fence
        // backwards updates no row, and the caller is told its effect is refused.
        let updated = connection
            .execute(
                "INSERT INTO aseman_core.coordination_fence (name, token) VALUES ($1, $2) \
                 ON CONFLICT (name) DO UPDATE SET token = EXCLUDED.token \
                 WHERE aseman_core.coordination_fence.token <= EXCLUDED.token",
                &[&name.as_str(), &token],
            )
            .map_err(db)?;
        if updated == 0 {
            return Err(PortError::Conflict);
        }
        Ok(())
    }
}
