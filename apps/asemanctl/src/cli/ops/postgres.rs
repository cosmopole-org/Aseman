//! PostgreSQL leg of the A902 backup, restore, and upgrade drivers.
//!
//! Core storage on PostgreSQL is backed up at the database level: the cluster's role
//! definitions (without passwords), a custom-format dump of the core database, and
//! one dump per live creature guest database named in
//! `aseman_core.guest_database_bindings` (ADR 0001). Each dump is a consistent
//! snapshot, so the node may keep running. `databases.json` records which databases
//! were captured and the row count of every core table, which restore re-checks.
//!
//! Restore only ever targets an explicitly configured, empty cluster: a core
//! database without the `aseman_core` schema and no pre-existing guest database of
//! the same name. It never drops or overwrites anything.
//!
//! The PostgreSQL client tools (`psql`, `pg_dump`, `pg_dumpall`, `pg_restore`) must be
//! on `PATH`; their major version must be at least the server's.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use aseman_config::{AsemanConfig, CoreStorageProvider};
use serde::{Deserialize, Serialize};

/// Where the PostgreSQL artifacts live inside a backup's `snapshot/` tree.
pub(super) const SNAPSHOT_DIR: &str = "postgres";

const CATALOG_FILE: &str = "databases.json";
const CORE_SCHEMA: &str = "aseman_core";

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

/// Dump the cluster globals, the core database, and every live guest database into
/// `directory`, which must not yet exist or be empty.
pub(super) fn dump(url: &str, directory: &Path) -> Result<Catalog> {
    fs::create_dir_all(directory.join("guest"))?;
    let core_database = database_name(url)?.to_owned();
    run(
        Command::new("pg_dumpall")
            .arg(format!("--dbname={url}"))
            .args(["--globals-only", "--no-role-passwords", "--file"])
            .arg(directory.join("globals.sql")),
        "pg_dumpall --globals-only",
    )?;
    run(
        Command::new("pg_dump")
            .arg(format!("--dbname={url}"))
            .args(["--format=custom", "--file"])
            .arg(directory.join("core.dump")),
        "pg_dump core database",
    )?;
    let guest_databases = guest_databases(url)?;
    for name in &guest_databases {
        run(
            Command::new("pg_dump")
                .arg(format!("--dbname={}", with_database(url, name)?))
                .args(["--format=custom", "--create", "--file"])
                .arg(directory.join("guest").join(format!("{name}.dump"))),
            "pg_dump guest database",
        )?;
    }
    let catalog = Catalog {
        core_database,
        guest_databases,
        core_row_counts: core_row_counts(url)?,
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
pub(super) fn ensure_empty_target(url: &str, catalog: &Catalog) -> Result<()> {
    let schema = query(
        url,
        &format!("SELECT count(*) FROM pg_namespace WHERE nspname = '{CORE_SCHEMA}'"),
    )?;
    if schema.trim() != "0" {
        bail!(
            "the target core database already contains the {CORE_SCHEMA} schema; restore \
             needs an empty target and never overwrites one"
        );
    }
    for name in &catalog.guest_databases {
        let exists = query(
            url,
            &format!(
                "SELECT count(*) FROM pg_database WHERE datname = '{}'",
                checked_identifier(name)?
            ),
        )?;
        if exists.trim() != "0" {
            bail!("the target already has a guest database named {name}");
        }
    }
    Ok(())
}

/// Restore globals, the core database, and every guest database from `directory`.
pub(super) fn restore(url: &str, directory: &Path, catalog: &Catalog) -> Result<()> {
    // Roles are cluster-wide; ones that already exist on the target (the restoring
    // role itself, for example) are reported and kept. Anything else is fatal.
    let output = Command::new("psql")
        .arg(format!("--dbname={url}"))
        .args(["--quiet", "--no-psqlrc", "-v", "ON_ERROR_STOP=0", "--file"])
        .arg(directory.join("globals.sql"))
        .output()
        .context("run psql (is the PostgreSQL client installed?)")?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let fatal: Vec<&str> = stderr
        .lines()
        .filter(|line| line.contains("ERROR:") && !line.contains("already exists"))
        .collect();
    if !output.status.success() || !fatal.is_empty() {
        bail!("restoring cluster roles failed: {}", fatal.join("; "));
    }
    run(
        Command::new("pg_restore")
            .arg(format!("--dbname={url}"))
            .arg("--exit-on-error")
            .arg(directory.join("core.dump")),
        "pg_restore core database",
    )?;
    let maintenance = with_database(url, "postgres")?;
    for name in &catalog.guest_databases {
        checked_identifier(name)?;
        run(
            Command::new("pg_restore")
                .arg(format!("--dbname={maintenance}"))
                .args(["--create", "--exit-on-error"])
                .arg(directory.join("guest").join(format!("{name}.dump"))),
            "pg_restore guest database",
        )?;
    }
    Ok(())
}

/// Check that the restored core database holds exactly the rows the backup counted,
/// and that every guest database exists.
pub(super) fn verify(url: &str, catalog: &Catalog) -> Result<()> {
    let actual = core_row_counts(url)?;
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
        let exists = query(
            url,
            &format!(
                "SELECT count(*) FROM pg_database WHERE datname = '{}'",
                checked_identifier(name)?
            ),
        )?;
        if exists.trim() != "1" {
            bail!("guest database {name} is missing after restore");
        }
    }
    Ok(())
}

fn guest_databases(url: &str) -> Result<Vec<String>> {
    let table = query(
        url,
        &format!("SELECT to_regclass('{CORE_SCHEMA}.guest_database_bindings') IS NOT NULL"),
    )?;
    if table.trim() != "t" {
        return Ok(Vec::new());
    }
    let rows = query(
        url,
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

fn core_row_counts(url: &str) -> Result<BTreeMap<String, u64>> {
    // `query_to_xml` counts every table in one round trip without dynamic SQL.
    let rows = query(
        url,
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

fn query(url: &str, sql: &str) -> Result<String> {
    let output = Command::new("psql")
        .arg(format!("--dbname={url}"))
        .args([
            "--no-psqlrc",
            "--tuples-only",
            "--no-align",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
        ])
        .arg(sql)
        .output()
        .context("run psql (is the PostgreSQL client installed?)")?;
    if !output.status.success() {
        bail!(
            "psql query failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn run(command: &mut Command, what: &str) -> Result<()> {
    let output = command
        .output()
        .with_context(|| format!("run {what} (is the PostgreSQL client installed?)"))?;
    if !output.status.success() {
        bail!(
            "{what} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
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
