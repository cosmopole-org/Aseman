//! PostgreSQL leg of the A902 backup, restore, and upgrade drivers.
//!
//! Core storage on PostgreSQL is backed up at the database level: the cluster's role
//! definitions (without passwords), a custom-format dump of the core database, and
//! one dump per live creature guest database named in
//! `aseman_core.guest_database_bindings` (ADR 0001). Each dump is a consistent
//! snapshot. `databases.json` records which databases were captured and the row count
//! of every core table, which restore re-checks.
//!
//! The databases are reached one of two ways ([`PgAccess`]): by URL from a node
//! configuration, with the PostgreSQL client tools on this host's `PATH` at a major
//! version no older than the server; or inside the compact deployment's `postgres`
//! service, which is unreachable from the host by design, so the tools run there and
//! the dumps stream through files here.
//!
//! Restore only ever targets an explicitly selected, empty cluster: a core database
//! without the `aseman_core` schema and no pre-existing guest database of the same
//! name. It never drops or overwrites anything.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use aseman_config::{AsemanConfig, CoreStorageProvider};
use serde::{Deserialize, Serialize};

use crate::cli::compact::Compact;

/// Where the PostgreSQL artifacts live inside a backup's `snapshot/` tree.
pub(super) const SNAPSHOT_DIR: &str = "postgres";

const CATALOG_FILE: &str = "databases.json";
const CORE_SCHEMA: &str = "aseman_core";
/// The compact profile's database owner and core database (`POSTGRES_USER`/`_DB`).
const COMPACT_USER: &str = "aseman";
const COMPACT_DATABASE: &str = "aseman";

/// How the PostgreSQL tools reach the databases.
#[derive(Clone, Debug)]
pub(super) enum PgAccess {
    /// A core-database URL; the tools run on this host.
    Url(String),
    /// The compact deployment's `postgres` service; the tools run inside it.
    Compact(Compact),
}

/// What a PostgreSQL backup captured.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Catalog {
    /// The core database name at the source.
    pub core_database: String,
    /// Live creature guest databases, each dumped to `guest/<name>.dump`.
    pub guest_databases: Vec<String>,
    /// Row count of every `aseman_core` table at dump time.
    pub core_row_counts: BTreeMap<String, u64>,
}

/// The core database URL when the node's core storage is PostgreSQL.
///
/// # Errors
///
/// When the provider is PostgreSQL but the URL secret is unset or unreadable.
pub(super) fn database_url(config: &AsemanConfig) -> Result<Option<String>> {
    if config.core_storage.provider != CoreStorageProvider::Postgres {
        return Ok(None);
    }
    let secret = config
        .database_url_secret
        .as_deref()
        .ok_or_else(|| anyhow!("ASEMAN_DATABASE_URL_SECRET is not configured"))?;
    let url = fs::read_to_string(secret)
        .with_context(|| format!("cannot read database URL secret {secret}"))?;
    let url = url.trim().to_owned();
    if url.is_empty() {
        bail!("database URL secret {secret} is empty");
    }
    Ok(Some(url))
}

impl PgAccess {
    fn core_database(&self) -> Result<String> {
        match self {
            Self::Url(url) => Ok(database_name(url)?.to_owned()),
            Self::Compact(_) => Ok(COMPACT_DATABASE.to_owned()),
        }
    }

    /// Run a PostgreSQL client `program` against `database`, streaming stdin from and
    /// stdout to files.
    fn run(
        &self,
        program: &str,
        database: &str,
        extra: &[&str],
        stdin: Option<&Path>,
        stdout: Option<&Path>,
    ) -> Result<Output> {
        match self {
            Self::Url(url) => {
                let mut command = Command::new(program);
                command
                    .arg(format!("--dbname={}", with_database(url, database)?))
                    .args(extra);
                if let Some(path) = stdin {
                    command.stdin(Stdio::from(
                        fs::File::open(path).with_context(|| format!("open {}", path.display()))?,
                    ));
                }
                if let Some(path) = stdout {
                    command.stdout(Stdio::from(
                        fs::File::create(path)
                            .with_context(|| format!("create {}", path.display()))?,
                    ));
                }
                let output = command.output().with_context(|| {
                    format!("run {program} (is the PostgreSQL client installed?)")
                })?;
                if !output.status.success() {
                    bail!(
                        "{program} failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }
                Ok(output)
            }
            Self::Compact(compact) => {
                let mut arguments = vec![program, "-U", COMPACT_USER];
                if program == "pg_dumpall" {
                    arguments.extend(["-l", database]);
                } else {
                    arguments.extend(["-d", database]);
                }
                arguments.extend_from_slice(extra);
                compact.exec_postgres(&arguments, stdin, stdout)
            }
        }
    }

    fn query(&self, database: &str, sql: &str) -> Result<String> {
        let output = self.run(
            "psql",
            database,
            &[
                "--no-psqlrc",
                "--tuples-only",
                "--no-align",
                "-v",
                "ON_ERROR_STOP=1",
                "-c",
                sql,
            ],
            None,
            None,
        )?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

/// Dump the cluster globals, the core database, and every live guest database into
/// `directory`.
pub(super) fn dump(access: &PgAccess, directory: &Path) -> Result<Catalog> {
    fs::create_dir_all(directory.join("guest"))?;
    let core_database = access.core_database()?;
    access.run(
        "pg_dumpall",
        &core_database,
        &["--globals-only", "--no-role-passwords"],
        None,
        Some(&directory.join("globals.sql")),
    )?;
    access.run(
        "pg_dump",
        &core_database,
        &["--format=custom"],
        None,
        Some(&directory.join("core.dump")),
    )?;
    let guest_databases = guest_databases(access, &core_database)?;
    for name in &guest_databases {
        access.run(
            "pg_dump",
            name,
            &["--format=custom", "--create"],
            None,
            Some(&directory.join("guest").join(format!("{name}.dump"))),
        )?;
    }
    let catalog = Catalog {
        core_row_counts: core_row_counts(access, &core_database)?,
        core_database,
        guest_databases,
    };
    fs::write(
        directory.join(CATALOG_FILE),
        serde_json::to_vec_pretty(&catalog)?,
    )?;
    Ok(catalog)
}

/// The catalog a backup recorded, if it captured PostgreSQL at all.
pub(super) fn read_catalog(directory: &Path) -> Result<Option<Catalog>> {
    let path = directory.join(CATALOG_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    Ok(Some(
        serde_json::from_str(&text).context("postgres databases.json is invalid")?,
    ))
}

/// Refuse a target that already holds Aseman core state or any of the guest
/// databases the backup would create.
pub(super) fn ensure_empty_target(access: &PgAccess, catalog: &Catalog) -> Result<()> {
    let core = access.core_database()?;
    let schema = access.query(
        &core,
        &format!("SELECT count(*) FROM pg_namespace WHERE nspname = '{CORE_SCHEMA}'"),
    )?;
    if schema.trim() != "0" {
        bail!(
            "the target core database already contains the {CORE_SCHEMA} schema; restore \
             needs an empty target and never overwrites one"
        );
    }
    for name in &catalog.guest_databases {
        if database_exists(access, &core, name)? {
            bail!("the target already has a guest database named {name}");
        }
    }
    Ok(())
}

/// Prepare a compact deployment to receive a restore. Its core schema already exists
/// (the node migrates on start), so it counts as empty when no core table holds a row
/// and no guest database exists; otherwise only an explicit `replace` proceeds. The
/// core database is then recreated and the deployment's own guest databases dropped,
/// so the restore lands on a clean catalog. Callers stop every writer first.
pub(super) fn prepare_compact_target(access: &PgAccess, replace: bool) -> Result<()> {
    let PgAccess::Compact(_) = access else {
        bail!("only a compact deployment is prepared in place");
    };
    let core = access.core_database()?;
    let rows: u64 = core_row_counts(access, &core)?.values().sum();
    let guests = guest_databases(access, &core)?;
    if (rows > 0 || !guests.is_empty()) && !replace {
        bail!(
            "the target deployment holds {rows} core row(s) and {} guest database(s); \
             pass --replace to overwrite it",
            guests.len()
        );
    }
    for name in &guests {
        access.query(
            "postgres",
            &format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                checked_identifier(name)?
            ),
        )?;
    }
    access.query(
        "postgres",
        &format!("DROP DATABASE {COMPACT_DATABASE} WITH (FORCE)"),
    )?;
    access.query(
        "postgres",
        &format!("CREATE DATABASE {COMPACT_DATABASE} OWNER {COMPACT_USER}"),
    )?;
    Ok(())
}

/// Restore globals, the core database, and every guest database from `directory`.
pub(super) fn restore(access: &PgAccess, directory: &Path, catalog: &Catalog) -> Result<()> {
    let core = access.core_database()?;
    // Roles are cluster-wide; ones that already exist on the target (the restoring
    // role itself, for example) are reported and kept. Anything else is fatal.
    let globals = access.run(
        "psql",
        &core,
        &["--quiet", "--no-psqlrc", "-v", "ON_ERROR_STOP=0"],
        Some(&directory.join("globals.sql")),
        None,
    )?;
    let stderr = String::from_utf8_lossy(&globals.stderr);
    let fatal: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("ERROR:") && !line.contains("already exists"))
        .collect();
    if !fatal.is_empty() {
        bail!("restoring cluster roles failed: {}", fatal.join("; "));
    }
    access.run(
        "pg_restore",
        &core,
        &["--exit-on-error"],
        Some(&directory.join("core.dump")),
        None,
    )?;
    for name in &catalog.guest_databases {
        checked_identifier(name)?;
        access.run(
            "pg_restore",
            "postgres",
            &["--create", "--exit-on-error"],
            Some(&directory.join("guest").join(format!("{name}.dump"))),
            None,
        )?;
    }
    Ok(())
}

/// Check that the restored core database holds exactly the rows the backup counted,
/// and that every guest database exists.
pub(super) fn verify(access: &PgAccess, catalog: &Catalog) -> Result<()> {
    let core = access.core_database()?;
    let actual = core_row_counts(access, &core)?;
    if actual != catalog.core_row_counts {
        let differing: Vec<&String> = catalog
            .core_row_counts
            .keys()
            .chain(actual.keys())
            .filter(|table| actual.get(*table) != catalog.core_row_counts.get(*table))
            .collect();
        bail!("restored core row counts differ for tables {differing:?}");
    }
    for name in &catalog.guest_databases {
        if !database_exists(access, &core, name)? {
            bail!("guest database {name} is missing after restore");
        }
    }
    Ok(())
}

fn database_exists(access: &PgAccess, core: &str, name: &str) -> Result<bool> {
    let count = access.query(
        core,
        &format!(
            "SELECT count(*) FROM pg_database WHERE datname = '{}'",
            checked_identifier(name)?
        ),
    )?;
    Ok(count.trim() == "1")
}

fn guest_databases(access: &PgAccess, core: &str) -> Result<Vec<String>> {
    let table = access.query(
        core,
        &format!("SELECT to_regclass('{CORE_SCHEMA}.guest_database_bindings') IS NOT NULL"),
    )?;
    if table.trim() != "t" {
        return Ok(Vec::new());
    }
    let rows = access.query(
        core,
        &format!(
            "SELECT DISTINCT b.database_name FROM {CORE_SCHEMA}.guest_database_bindings b \
             JOIN pg_database d ON d.datname = b.database_name \
             WHERE NOT b.tombstone ORDER BY 1"
        ),
    )?;
    rows.lines()
        .filter(|line| !line.is_empty())
        .map(|name| checked_identifier(name).map(str::to_owned))
        .collect()
}

fn core_row_counts(access: &PgAccess, core: &str) -> Result<BTreeMap<String, u64>> {
    // `query_to_xml` counts every table in one round trip without dynamic SQL.
    let rows = access.query(
        core,
        &format!(
            "SELECT c.relname || '|' || (xpath('/row/n/text()', query_to_xml(format(\
             'SELECT count(*) AS n FROM {CORE_SCHEMA}.%I', c.relname), false, true, '')))[1]::text \
             FROM pg_class c JOIN pg_namespace s ON s.oid = c.relnamespace \
             WHERE s.nspname = '{CORE_SCHEMA}' AND c.relkind IN ('r', 'p') ORDER BY 1"
        ),
    )?;
    let mut counts = BTreeMap::new();
    for line in rows.lines().filter(|line| !line.is_empty()) {
        let (table, count) = line
            .split_once('|')
            .ok_or_else(|| anyhow!("unexpected row count line {line:?}"))?;
        counts.insert(table.to_owned(), count.parse()?);
    }
    Ok(counts)
}

/// Guest database names come from the binding table and are interpolated into SQL and
/// file names, so only the provisioner's own lowercase identifier shape is accepted.
fn checked_identifier(name: &str) -> Result<&str> {
    let valid = !name.is_empty()
        && name.len() <= 63
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if valid {
        Ok(name)
    } else {
        Err(anyhow!("refusing unexpected database name {name:?}"))
    }
}

/// The database a `postgresql://` URL names.
fn database_name(url: &str) -> Result<&str> {
    let (_, path) = split_path(url)?;
    let name = path.split('?').next().unwrap_or_default();
    if name.is_empty() {
        bail!("the database URL names no database");
    }
    Ok(name)
}

/// The same URL naming another database on the same server.
fn with_database(url: &str, database: &str) -> Result<String> {
    let (authority, path) = split_path(url)?;
    let query = path.split_once('?').map(|(_, query)| query);
    Ok(match query {
        Some(query) => format!("{authority}/{database}?{query}"),
        None => format!("{authority}/{database}"),
    })
}

fn split_path(url: &str) -> Result<(&str, &str)> {
    let scheme_end = url
        .find("://")
        .ok_or_else(|| anyhow!("the database URL is not a postgresql:// URL"))?
        + 3;
    let slash = url[scheme_end..]
        .find('/')
        .map(|index| scheme_end + index)
        .ok_or_else(|| anyhow!("the database URL names no database"))?;
    Ok((&url[..slash], &url[slash + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn database_urls_are_retargeted_without_touching_credentials_or_options() {
        let url = "postgresql://aseman:p%40ss@db:5432/aseman?sslmode=require";
        assert_eq!(database_name(url).unwrap(), "aseman");
        assert_eq!(
            with_database(url, "postgres").unwrap(),
            "postgresql://aseman:p%40ss@db:5432/postgres?sslmode=require"
        );
        assert_eq!(
            with_database("postgres://u@h/core", "g_1").unwrap(),
            "postgres://u@h/g_1"
        );
        assert!(database_name("postgresql://u@h/").is_err());
        assert!(database_name("not a url").is_err());
    }

    #[test]
    fn only_provisioner_shaped_database_names_are_accepted() {
        assert!(checked_identifier("aseman_guest_0123abcd").is_ok());
        for bad in ["", "Guest", "a;DROP", "a'b", "../x", &"a".repeat(64)] {
            assert!(checked_identifier(bad).is_err(), "{bad}");
        }
    }
}
