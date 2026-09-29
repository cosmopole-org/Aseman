//! PostgreSQL plumbing shared by the adapters that own a PostgreSQL schema (the
//! storage provider, the VMM store, coordination, finance, realtime, and
//! federation): one pool type, how a pool is built and a connection checked out,
//! and the one mapping from a driver error to a [`PortError`].
//!
//! Adapters use it; domain, ports, and application code never name it.

use aseman_ports::{PortError, PortResult};
use postgres::NoTls;
use postgres::error::SqlState;

/// The connection manager every adapter pool uses.
pub type Manager = r2d2_postgres::PostgresConnectionManager<NoTls>;
/// A pool of PostgreSQL connections.
pub type Pool = r2d2::Pool<Manager>;
/// A connection checked out of a [`Pool`].
pub type Connection = r2d2::PooledConnection<Manager>;

/// A pool builder holding at most `max_size` connections (at least one), for
/// callers that tune the pool further.
#[must_use]
pub fn pool_builder(max_size: u32) -> r2d2::Builder<Manager> {
    Pool::builder().max_size(max_size.max(1))
}

/// A pool of at most `max_size` connections (at least one) to `config`.
///
/// # Errors
///
/// The pool's error when its first connection cannot be opened.
pub fn pool(config: postgres::Config, max_size: u32) -> Result<Pool, r2d2::Error> {
    pool_builder(max_size).build(Manager::new(config, NoTls))
}

/// A port adapter's pool.
///
/// # Errors
///
/// `Unavailable(database)` when the pool cannot be built.
pub fn port_pool(
    config: postgres::Config,
    max_size: u32,
    database: &'static str,
) -> PortResult<Pool> {
    pool(config, max_size).map_err(|_| PortError::Unavailable(database))
}

/// Check a connection out of `pool`.
///
/// # Errors
///
/// `Unavailable("postgres")` when no connection can be had.
pub fn connection(pool: &Pool) -> PortResult<Connection> {
    pool.get().map_err(|_| PortError::Unavailable("postgres"))
}

/// A driver error as a port error: a unique violation is a `Conflict`, a closed
/// connection makes the database `Unavailable`, and anything else `Failed`.
#[must_use]
pub fn port_error(error: postgres::Error) -> PortError {
    match error.code() {
        Some(code) if *code == SqlState::UNIQUE_VIOLATION => PortError::Conflict,
        _ if error.is_closed() => PortError::Unavailable("postgres"),
        _ => PortError::failed(error),
    }
}
