//! PostgreSQL plumbing shared by the adapters that own a PostgreSQL schema (the
//! storage provider, the VMM store, coordination, finance, realtime, and
//! federation): one pool type, how a pool is built and a connection checked out,
//! and the one mapping from a driver error to a [`PortError`].
//!
//! Adapters use it; domain, ports, and application code never name it.

use aseman_ports::{PortError, PortResult};
use postgres::error::SqlState;

mod tls;

pub use tls::TlsConnector;

/// The connection manager every adapter pool uses.
pub type Manager = r2d2_postgres::PostgresConnectionManager<TlsConnector>;
/// A pool of PostgreSQL connections.
pub type Pool = r2d2::Pool<Manager>;
/// A connection checked out of a [`Pool`].
pub type Connection = r2d2::PooledConnection<Manager>;

/// A PostgreSQL database: its connection settings and the TLS trust its URL names.
///
/// URLs take libpq's form. Besides the driver's options, `sslrootcert=FILE` adds a CA
/// (PEM) to the trusted roots; certificates are always verified, host name included.
#[derive(Clone, Debug)]
pub struct Database {
    config: postgres::Config,
    tls: TlsConnector,
}

impl Database {
    /// # Errors
    ///
    /// An invalid URL, or an unreadable `sslrootcert`.
    pub fn parse(url: &str) -> Result<Self, String> {
        let url = url.trim();
        let (driver_url, root) =
            if url.starts_with("postgres://") || url.starts_with("postgresql://") {
                let mut parsed = url::Url::parse(url)
                    .map_err(|error| format!("invalid database URL: {error}"))?;
                let mut root = None;
                let kept: Vec<(String, String)> = parsed
                    .query_pairs()
                    .filter_map(|(key, value)| {
                        if key == "sslrootcert" {
                            root = Some(std::path::PathBuf::from(value.as_ref()));
                            None
                        } else {
                            Some((key.into_owned(), value.into_owned()))
                        }
                    })
                    .collect();
                if kept.is_empty() {
                    parsed.set_query(None);
                } else {
                    parsed.query_pairs_mut().clear().extend_pairs(kept);
                }
                (parsed.to_string(), root)
            } else {
                if url.contains("sslrootcert") {
                    return Err("sslrootcert is supported in the postgres:// URL form".to_owned());
                }
                (url.to_owned(), None)
            };
        let config = driver_url
            .parse::<postgres::Config>()
            .map_err(|error| format!("invalid database URL: {error}"))?;
        Ok(Self {
            config,
            tls: TlsConnector::new(root.as_deref())?,
        })
    }

    #[must_use]
    pub fn config(&self) -> &postgres::Config {
        &self.config
    }

    /// The settings to adjust before connecting (for example another database name).
    pub fn config_mut(&mut self) -> &mut postgres::Config {
        &mut self.config
    }

    /// A connection manager for pools of this database.
    #[must_use]
    pub fn manager(&self) -> Manager {
        Manager::new(self.config.clone(), self.tls.clone())
    }

    /// One client, outside any pool.
    ///
    /// # Errors
    ///
    /// The connection or its TLS handshake fails.
    pub fn connect(&self) -> Result<postgres::Client, postgres::Error> {
        self.config.connect(self.tls.clone())
    }
}

/// A pool builder holding at most `max_size` connections (at least one), for
/// callers that tune the pool further.
#[must_use]
pub fn pool_builder(max_size: u32) -> r2d2::Builder<Manager> {
    Pool::builder().max_size(max_size.max(1))
}

/// A pool of at most `max_size` connections (at least one) to `database`.
///
/// # Errors
///
/// The pool's error when its first connection cannot be opened.
pub fn pool(database: &Database, max_size: u32) -> Result<Pool, r2d2::Error> {
    pool_builder(max_size).build(database.manager())
}

/// A port adapter's pool.
///
/// # Errors
///
/// `Unavailable(label)` when the pool cannot be built.
pub fn port_pool(database: &Database, max_size: u32, label: &'static str) -> PortResult<Pool> {
    pool(database, max_size).map_err(|_| PortError::Unavailable(label))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sslrootcert_is_taken_from_the_url_and_the_rest_reaches_the_driver() {
        let ca =
            std::env::temp_dir().join(format!("aseman-postgres-ca-{}.pem", std::process::id()));
        // Any valid certificate will do as a trust root for parsing.
        std::fs::write(&ca, TEST_CA).unwrap();
        let url = format!(
            "postgres://aseman:p%40ss@db.example:6432/aseman?sslmode=require&sslrootcert={}",
            ca.display()
        );
        let database = Database::parse(&url).unwrap();
        assert_eq!(database.config().get_dbname(), Some("aseman"));
        assert_eq!(database.config().get_password(), Some(&b"p@ss"[..]));
        assert_eq!(
            database.config().get_ssl_mode(),
            postgres::config::SslMode::Require
        );
        std::fs::remove_file(ca).unwrap();
    }

    #[test]
    fn a_trust_root_that_cannot_be_used_is_an_error() {
        assert!(Database::parse("postgres://a@h/d?sslrootcert=/absent/ca.pem").is_err());
        let empty =
            std::env::temp_dir().join(format!("aseman-postgres-empty-{}.pem", std::process::id()));
        std::fs::write(&empty, "not a certificate").unwrap();
        assert!(
            Database::parse(&format!("postgres://a@h/d?sslrootcert={}", empty.display())).is_err()
        );
        std::fs::remove_file(empty).unwrap();
        assert!(Database::parse("host=h user=a sslrootcert=/ca.pem").is_err());
        assert!(Database::parse("host=h user=a sslmode=disable").is_ok());
    }

    /// A self-signed test CA certificate (no key is kept anywhere).
    const TEST_CA: &str = "\
-----BEGIN CERTIFICATE-----\n\
MIIBlDCCATmgAwIBAgIUPlz1iECE0zUBFlB5GPBYLGRqVaEwCgYIKoZIzj0EAwIw\n\
HjEcMBoGA1UEAwwTYXNlbWFuLXVuaXQtdGVzdC1jYTAgFw0yNjA5MjkxMDIxMzRa\n\
GA8yMTI2MDkwNTEwMjEzNFowHjEcMBoGA1UEAwwTYXNlbWFuLXVuaXQtdGVzdC1j\n\
YTBZMBMGByqGSM49AgEGCCqGSM49AwEHA0IABEe6vwrKLwulhfI6fUShFFNt/uUx\n\
9KfenegPM8+iEdbgY1+gZVDR3R3+gpTkYc335QT3rpGRH5Y88DxsHmMGuJujUzBR\n\
MB0GA1UdDgQWBBSmEKwFLvXEmyNA4FLS1zNS9dxu4jAfBgNVHSMEGDAWgBSmEKwF\n\
LvXEmyNA4FLS1zNS9dxu4jAPBgNVHRMBAf8EBTADAQH/MAoGCCqGSM49BAMCA0kA\n\
MEYCIQCGRQ028rUqPqUMYIkOLwK5Aeptz49L28fXSXuceB2TGwIhAMreGB/RhtkE\n\
D432ucESgeXdXwobC27R9oe0pqTDvAJp\n\
-----END CERTIFICATE-----\n";
}
