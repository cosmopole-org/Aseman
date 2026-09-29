//! Cluster mode of the PostgreSQL provider (ADR 0033): sharding with replication.
//!
//! A [`ShardMap`] names the shards; each has a primary and zero or more streaming
//! replicas (replication itself is PostgreSQL's, operated with the database). Capsule
//! kinds split two ways:
//!
//! - **Reference kinds** — referenced by another kind or carrying a uniqueness
//!   constraint beyond their id — are written to every shard and read from the home
//!   shard, so every foreign key and unique index is enforced by PostgreSQL on each
//!   shard exactly as on one database.
//! - **Distributed kinds** — the high-volume leaves: samples, logs, audit, realtime,
//!   ledger and usage records — live on the shard a jump consistent hash of the
//!   capsule id selects. Point reads and writes touch that shard; queries fan out and
//!   merge in the provider's order.
//!
//! A unit of work that touched one shard commits normally. One that touched several
//! commits with two-phase commit: every shard prepares, the coordinator records its
//! decision on the home shard, then every shard commits. [`ShardedUnitOfWorkFactory`]
//! resolves prepared transactions a crashed coordinator left behind from that record
//! (presumed abort), when it starts and on [`ShardedUnitOfWorkFactory::recover`].

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use aseman_capsule::{CapsuleStore, CapsuleStoreError, CapsuleStoreResult};
use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::unit_of_work::{PostgresUnitOfWork, PostgresUnitOfWorkFactory, PreparedUnit};
use crate::{
    PostgresCapsuleRepository, PostgresStorageError, SortValue, StorageResult, is_reference_kind,
    map_postgres_error,
};

/// Prepared transactions older than this with no recorded commit are rolled back.
const ABANDONED_AFTER: Duration = Duration::from_secs(60);
const DECISIONS: &str = "aseman_core.shard_commit_decisions";
const GID_PREFIX: &str = "aseman-";

/// One shard: a primary and its read replicas.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ShardSpec {
    pub name: String,
    pub primary: String,
    #[serde(default)]
    pub replicas: Vec<String>,
    /// The trusted guest proxy's connection to this shard's server (A306), for the
    /// creature guest databases placed here.
    #[serde(default)]
    pub guest_proxy: Option<String>,
}

/// The versioned shard map (`ASEMAN_POSTGRES_SHARDS_SECRET`).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ShardMap {
    pub version: u64,
    /// The shard holding reference-kind reads, coordination state, and the
    /// two-phase-commit decisions.
    pub home: String,
    /// Serve read-only units from replicas (their lag is visible to such reads).
    #[serde(default)]
    pub read_from_replicas: bool,
    pub shards: Vec<ShardSpec>,
}

impl ShardMap {
    /// The one-shard map of a single database.
    #[must_use]
    pub fn single(connection_uri: &str) -> Self {
        Self {
            version: 1,
            home: "primary".to_owned(),
            read_from_replicas: false,
            shards: vec![ShardSpec {
                name: "primary".to_owned(),
                primary: connection_uri.to_owned(),
                replicas: Vec::new(),
                guest_proxy: None,
            }],
        }
    }

    /// Parse and validate a shard map document.
    pub fn parse(json: &str) -> StorageResult<Self> {
        let map: Self = serde_json::from_str(json)
            .map_err(|error| PostgresStorageError::Invalid(format!("shard map: {error}")))?;
        map.validate()?;
        Ok(map)
    }

    fn validate(&self) -> StorageResult<()> {
        let invalid = |reason: &str| {
            Err(PostgresStorageError::Invalid(format!(
                "shard map: {reason}"
            )))
        };
        if self.shards.is_empty() {
            return invalid("no shards");
        }
        let mut names = std::collections::BTreeSet::new();
        for shard in &self.shards {
            if shard.name.is_empty() || !names.insert(shard.name.as_str()) {
                return invalid("shard names must be present and unique");
            }
        }
        if !names.contains(self.home.as_str()) {
            return invalid("the home shard is not listed");
        }
        Ok(())
    }

    pub fn home_index(&self) -> usize {
        self.shards
            .iter()
            .position(|shard| shard.name == self.home)
            .unwrap_or(0)
    }
}

/// The shard a capsule id belongs to among `shards` (jump consistent hash: growing
/// the map moves only the capsules the new shard takes).
#[must_use]
pub fn shard_of(id: &CapsuleId, shards: usize) -> usize {
    let digest = Sha256::digest(id.0);
    let mut key = u64::from_be_bytes(digest[..8].try_into().expect("eight bytes"));
    let buckets = i64::try_from(shards.max(1)).unwrap_or(i64::MAX);
    let (mut bucket, mut next) = (-1_i64, 0_i64);
    while next < buckets {
        bucket = next;
        key = key.wrapping_mul(2_862_933_555_777_941_757).wrapping_add(1);
        let factor =
            f64::from(1_u32 << 31) / f64::from(u32::try_from((key >> 33) + 1).unwrap_or(u32::MAX));
        next = ((bucket + 1) as f64 * factor) as i64;
    }
    usize::try_from(bucket).unwrap_or(0)
}

/// An open transaction on the selected provider.
pub trait UnitOfWork: CapsuleStore {
    /// Live capsules of `kind` matching a model query (ADR 0036).
    fn find(
        &self,
        kind: &str,
        query: &aseman_storage::FindMany,
    ) -> StorageResult<Vec<CapsuleEnvelope>>;
    /// How many live capsules of `kind` match `filter`.
    fn count(&self, kind: &str, filter: Option<&aseman_storage::Where>) -> StorageResult<u64>;
    /// Commit every write of the unit, on every shard it touched. The unit is
    /// finished afterwards.
    fn commit(&self) -> StorageResult<()>;
    /// Discard every write of the unit.
    fn rollback(&self) -> StorageResult<()>;
}

/// Opens units of work.
pub trait UnitOfWorkFactory: Send + Sync {
    fn begin(&self) -> StorageResult<Box<dyn UnitOfWork>>;
    /// A unit for a read-only action; a cluster may serve it from a replica.
    fn begin_read_only(&self) -> StorageResult<Box<dyn UnitOfWork>> {
        self.begin()
    }
}

impl UnitOfWork for PostgresUnitOfWork {
    fn find(
        &self,
        kind: &str,
        query: &aseman_storage::FindMany,
    ) -> StorageResult<Vec<CapsuleEnvelope>> {
        self.with_client(|client| crate::model_query::find_on(client, kind, query))
    }

    fn count(&self, kind: &str, filter: Option<&aseman_storage::Where>) -> StorageResult<u64> {
        self.with_client(|client| crate::model_query::count_on(client, kind, filter))
    }

    fn commit(&self) -> StorageResult<()> {
        self.finish("COMMIT")
    }

    fn rollback(&self) -> StorageResult<()> {
        self.finish("ROLLBACK")
    }
}

impl UnitOfWorkFactory for PostgresUnitOfWorkFactory {
    fn begin(&self) -> StorageResult<Box<dyn UnitOfWork>> {
        PostgresUnitOfWorkFactory::begin(self).map(|unit| Box::new(unit) as Box<dyn UnitOfWork>)
    }
}

struct Shard {
    primary: PostgresUnitOfWorkFactory,
    replicas: Vec<PostgresUnitOfWorkFactory>,
    admin: String,
}

/// Units of work over a sharded, replicated PostgreSQL cluster.
pub struct ShardedUnitOfWorkFactory {
    map: ShardMap,
    shards: Vec<Shard>,
    home: usize,
    next_replica: std::sync::atomic::AtomicUsize,
}

impl ShardedUnitOfWorkFactory {
    /// Connect every shard, migrate it to `layout`, and resolve prepared transactions a previous
    /// coordinator left behind.
    pub fn connect(
        map: ShardMap,
        max_connections_per_shard: u32,
        generation: Option<u64>,
        layout: aseman_config::CapsuleLayout,
    ) -> StorageResult<Self> {
        map.validate()?;
        let mut shards = Vec::with_capacity(map.shards.len());
        for spec in &map.shards {
            // Every shard holds its rows in the cluster's one layout (ADR 0034).
            PostgresCapsuleRepository::connect(&spec.primary)?.migrate_layout(layout)?;
            let mut replicas = Vec::with_capacity(spec.replicas.len());
            for replica in &spec.replicas {
                replicas.push(PostgresUnitOfWorkFactory::connect(
                    replica,
                    max_connections_per_shard,
                    generation,
                )?);
            }
            shards.push(Shard {
                primary: PostgresUnitOfWorkFactory::connect(
                    &spec.primary,
                    max_connections_per_shard,
                    generation,
                )?,
                replicas,
                admin: spec.primary.clone(),
            });
        }
        let home = map.home_index();
        let factory = Self {
            map,
            shards,
            home,
            next_replica: std::sync::atomic::AtomicUsize::new(0),
        };
        factory.admin(factory.home)?.batch_execute(&format!(
            "CREATE TABLE IF NOT EXISTS {DECISIONS} (gid text PRIMARY KEY, decided_at timestamptz NOT NULL DEFAULT now())"
        )).map_err(map_postgres_error)?;
        factory.recover()?;
        Ok(factory)
    }

    #[must_use]
    pub fn shard_map(&self) -> &ShardMap {
        &self.map
    }

    fn admin(&self, shard: usize) -> StorageResult<postgres::Client> {
        aseman_postgres::Database::parse(&self.shards[shard].admin)
            .map_err(crate::PostgresStorageError::Unavailable)?
            .connect()
            .map_err(map_postgres_error)
    }

    /// Resolve every prepared Aseman transaction: commit the ones whose decision is
    /// recorded, roll back the undecided ones old enough to be abandoned, and forget
    /// decisions no shard still needs. Returns how many transactions it resolved.
    pub fn recover(&self) -> StorageResult<usize> {
        self.recover_older_than(ABANDONED_AFTER)
    }

    /// [`Self::recover`] with an explicit abandonment age (operators and tests).
    pub fn recover_older_than(&self, abandoned_after: Duration) -> StorageResult<usize> {
        let mut home = self.admin(self.home)?;
        let mut resolved = 0;
        for shard in 0..self.shards.len() {
            let mut client = self.admin(shard)?;
            let prepared = client
                .query(
                    "SELECT gid, extract(epoch FROM now() - prepared)::float8 FROM pg_prepared_xacts \
                     WHERE database = current_database() AND gid LIKE 'aseman-%'",
                    &[],
                )
                .map_err(map_postgres_error)?;
            for row in prepared {
                let gid: String = row.get(0);
                let age: f64 = row.get(1);
                // `<decision id>-<shard>`: the decision is recorded under the base id.
                let decision = gid.rsplit_once('-').map_or(gid.as_str(), |(base, _)| base);
                let decided = home
                    .query_opt(
                        &format!("SELECT 1 FROM {DECISIONS} WHERE gid = $1"),
                        &[&decision],
                    )
                    .map_err(map_postgres_error)?
                    .is_some();
                let statement = if decided {
                    format!("COMMIT PREPARED '{gid}'")
                } else if age >= abandoned_after.as_secs_f64() {
                    format!("ROLLBACK PREPARED '{gid}'")
                } else {
                    continue;
                };
                client
                    .batch_execute(&statement)
                    .map_err(map_postgres_error)?;
                resolved += 1;
            }
        }
        home.batch_execute(&format!(
            "DELETE FROM {DECISIONS} WHERE decided_at < now() - interval '1 day'"
        ))
        .map_err(map_postgres_error)?;
        Ok(resolved)
    }

    /// A read-only unit, from a replica when the map allows it. Writes through it fail.
    pub fn begin_read_only(self: &std::sync::Arc<Self>) -> StorageResult<ShardedUnitOfWork> {
        let unit = self.begin_sharded()?;
        if self.map.read_from_replicas {
            *unit
                .read_replicas
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = true;
        }
        Ok(unit)
    }

    fn begin_sharded(self: &std::sync::Arc<Self>) -> StorageResult<ShardedUnitOfWork> {
        Ok(ShardedUnitOfWork {
            factory: self.clone(),
            units: Mutex::new(BTreeMap::new()),
            read_replicas: Mutex::new(false),
        })
    }
}

/// One unit of work across the shards it touches; each shard's transaction opens on
/// first use.
pub struct ShardedUnitOfWork {
    factory: std::sync::Arc<ShardedUnitOfWorkFactory>,
    units: Mutex<BTreeMap<usize, PostgresUnitOfWork>>,
    read_replicas: Mutex<bool>,
}

fn store_error(error: PostgresStorageError) -> CapsuleStoreError {
    match error {
        PostgresStorageError::Conflict => CapsuleStoreError::Conflict,
        other => CapsuleStoreError::Failed(other.to_string()),
    }
}

impl ShardedUnitOfWork {
    fn with_unit<T>(
        &self,
        shard: usize,
        operation: impl FnOnce(&PostgresUnitOfWork) -> CapsuleStoreResult<T>,
    ) -> CapsuleStoreResult<T> {
        let mut units = self.units.lock().unwrap_or_else(|error| error.into_inner());
        if let std::collections::btree_map::Entry::Vacant(slot) = units.entry(shard) {
            let replicas = &self.factory.shards[shard].replicas;
            let from_replica = *self
                .read_replicas
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                && !replicas.is_empty();
            let factory = if from_replica {
                let next = self
                    .factory
                    .next_replica
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                &replicas[next % replicas.len()]
            } else {
                &self.factory.shards[shard].primary
            };
            slot.insert(factory.begin().map_err(store_error)?);
        }
        operation(units.get(&shard).expect("unit was just opened"))
    }

    fn reference(kind: &CapsuleKind) -> CapsuleStoreResult<bool> {
        is_reference_kind(kind).map_err(store_error)
    }

    fn owning_shard(&self, id: &CapsuleId) -> usize {
        shard_of(id, self.factory.shards.len())
    }

    /// Two-phase commit across every touched shard.
    fn commit_all(&self) -> StorageResult<()> {
        let units =
            std::mem::take(&mut *self.units.lock().unwrap_or_else(|error| error.into_inner()));
        if units.len() <= 1 {
            return units
                .into_values()
                .next()
                .map_or(Ok(()), PostgresUnitOfWork::commit);
        }
        let gid = format!("{GID_PREFIX}{}", uuid::Uuid::now_v7().simple());
        let mut prepared: Vec<PreparedUnit> = Vec::with_capacity(units.len());
        let mut pending = units.into_iter();
        // Prepared-transaction ids are server-wide, and shards may share a server:
        // each shard prepares its own id under the unit's decision id.
        for (shard, unit) in pending.by_ref() {
            match unit.prepare(&format!("{gid}-{shard}")) {
                Ok(unit) => prepared.push(unit),
                Err(error) => {
                    for unit in prepared {
                        let _ = unit.rollback();
                    }
                    for (_, unit) in pending {
                        let _ = unit.rollback();
                    }
                    return Err(error);
                }
            }
        }
        // The decision is durable before any shard commits; a crash after this point
        // is finished by recovery, a crash before it is rolled back by recovery.
        let recorded = self.factory.admin(self.factory.home).and_then(|mut home| {
            home.execute(
                &format!("INSERT INTO {DECISIONS} (gid) VALUES ($1)"),
                &[&gid],
            )
            .map_err(map_postgres_error)
        });
        if let Err(error) = recorded {
            for unit in prepared {
                let _ = unit.rollback();
            }
            return Err(error);
        }
        let mut first_error = None;
        for unit in prepared {
            if let Err(error) = unit.commit() {
                // Decided: recovery completes it; the commit stands.
                first_error.get_or_insert(error);
            }
        }
        if let Some(error) = first_error {
            eprintln!("shard commit {gid} is decided; recovery will finish it: {error}");
        }
        Ok(())
    }
}

impl CapsuleStore for ShardedUnitOfWork {
    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> CapsuleStoreResult<Option<CapsuleEnvelope>> {
        let shard = if Self::reference(kind)? {
            self.factory.home
        } else {
            self.owning_shard(id)
        };
        self.with_unit(shard, |unit| unit.get(kind, id))
    }

    fn put(
        &self,
        capsule: &CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> CapsuleStoreResult<()> {
        self.put_all(&[(capsule.clone(), expected_revision)])
    }

    fn put_all(&self, writes: &[(CapsuleEnvelope, Option<u64>)]) -> CapsuleStoreResult<()> {
        let mut by_shard: BTreeMap<usize, Vec<(CapsuleEnvelope, Option<u64>)>> = BTreeMap::new();
        for (capsule, expected) in writes {
            if Self::reference(&capsule.kind)? {
                for shard in 0..self.factory.shards.len() {
                    by_shard
                        .entry(shard)
                        .or_default()
                        .push((capsule.clone(), *expected));
                }
            } else {
                by_shard
                    .entry(self.owning_shard(&capsule.id))
                    .or_default()
                    .push((capsule.clone(), *expected));
            }
        }
        for (shard, writes) in by_shard {
            self.with_unit(shard, |unit| unit.put_all(&writes))?;
        }
        Ok(())
    }

    fn query(&self, query: &CapsuleQuery) -> CapsuleStoreResult<Vec<CapsuleEnvelope>> {
        if Self::reference(&query.kind)? {
            return self.with_unit(self.factory.home, |unit| unit.query(query));
        }
        let mut rows: Vec<(Vec<SortValue>, CapsuleEnvelope)> = Vec::new();
        for shard in 0..self.factory.shards.len() {
            rows.extend(
                self.with_unit(shard, |unit| unit.query_keyed(query).map_err(store_error))?,
            );
        }
        let directions: Vec<bool> = query
            .sort
            .iter()
            .map(|sort| {
                matches!(
                    sort.direction,
                    aseman_contracts::capsule::SortDirection::Descending
                )
            })
            .chain(std::iter::once(false))
            .collect();
        rows.sort_by(|(left, _), (right, _)| {
            for ((left, right), descending) in left.iter().zip(right).zip(&directions) {
                let order = left.compare(right);
                let order = if *descending { order.reverse() } else { order };
                if order != std::cmp::Ordering::Equal {
                    return order;
                }
            }
            std::cmp::Ordering::Equal
        });
        rows.truncate(usize::try_from(query.limit).unwrap_or(usize::MAX));
        Ok(rows.into_iter().map(|(_, capsule)| capsule).collect())
    }
}

impl UnitOfWork for ShardedUnitOfWork {
    fn find(
        &self,
        kind: &str,
        query: &aseman_storage::FindMany,
    ) -> StorageResult<Vec<CapsuleEnvelope>> {
        let capsule_kind = CapsuleKind(kind.to_owned());
        let failed =
            |error: CapsuleStoreError| PostgresStorageError::Unavailable(error.to_string());
        if is_reference_kind(&capsule_kind)? {
            return self
                .with_unit(self.factory.home, |unit| {
                    unit.find(kind, query).map_err(store_error)
                })
                .map_err(failed);
        }
        // Each shard returns its first `skip + take` rows; the merge applies `skip`.
        let window = query
            .skip
            .saturating_add(query.take.unwrap_or(crate::model_query::DEFAULT_TAKE));
        let mut rows = Vec::new();
        for shard in 0..self.factory.shards.len() {
            rows.extend(
                self.with_unit(shard, |unit| {
                    unit.with_client(|client| {
                        crate::model_query::find_keyed_on(client, kind, query, 0, window)
                    })
                    .map_err(store_error)
                })
                .map_err(failed)?,
            );
        }
        Ok(crate::model_query::merge(rows, query))
    }

    fn count(&self, kind: &str, filter: Option<&aseman_storage::Where>) -> StorageResult<u64> {
        let failed =
            |error: CapsuleStoreError| PostgresStorageError::Unavailable(error.to_string());
        if is_reference_kind(&CapsuleKind(kind.to_owned()))? {
            return self
                .with_unit(self.factory.home, |unit| {
                    unit.count(kind, filter).map_err(store_error)
                })
                .map_err(failed);
        }
        let mut total = 0;
        for shard in 0..self.factory.shards.len() {
            total += self
                .with_unit(shard, |unit| unit.count(kind, filter).map_err(store_error))
                .map_err(failed)?;
        }
        Ok(total)
    }

    fn commit(&self) -> StorageResult<()> {
        self.commit_all()
    }

    fn rollback(&self) -> StorageResult<()> {
        let units =
            std::mem::take(&mut *self.units.lock().unwrap_or_else(|error| error.into_inner()));
        for (_, unit) in units {
            unit.rollback()?;
        }
        Ok(())
    }
}

impl UnitOfWorkFactory for std::sync::Arc<ShardedUnitOfWorkFactory> {
    fn begin(&self) -> StorageResult<Box<dyn UnitOfWork>> {
        Ok(Box::new(self.begin_sharded()?))
    }

    fn begin_read_only(&self) -> StorageResult<Box<dyn UnitOfWork>> {
        Ok(Box::new(ShardedUnitOfWorkFactory::begin_read_only(self)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jump_hash_is_stable_and_moves_little_when_a_shard_is_added() {
        let ids: Vec<CapsuleId> = (0..2_000)
            .map(|_| CapsuleId(*uuid::Uuid::now_v7().as_bytes()))
            .collect();
        for id in &ids {
            assert_eq!(shard_of(id, 4), shard_of(id, 4));
            assert!(shard_of(id, 4) < 4);
            assert_eq!(shard_of(id, 1), 0);
        }
        let moved = ids
            .iter()
            .filter(|id| shard_of(id, 4) != shard_of(id, 5))
            .count();
        // Ideally 1/5 move; allow statistical slack.
        assert!(moved < ids.len() * 3 / 10, "{moved} of {} moved", ids.len());
        let on_new_shard = ids.iter().filter(|id| shard_of(id, 5) == 4).count();
        assert_eq!(
            moved, on_new_shard,
            "only capsules the new shard takes move"
        );
    }

    #[test]
    fn every_foreign_key_target_is_a_reference_kind_and_leaves_are_distributed() {
        let tables = crate::all_tables().unwrap();
        let mut distributed = Vec::new();
        for table in tables {
            let kind = CapsuleKind(table.kind.clone());
            if !is_reference_kind(&kind).unwrap() {
                distributed.push(table.kind.clone());
            }
            for relationship in table.relationships.values() {
                assert!(
                    is_reference_kind(&CapsuleKind(relationship.target_kind.clone())).unwrap(),
                    "{} references distributed {}",
                    table.kind,
                    relationship.target_kind
                );
            }
        }
        eprintln!("distributed kinds: {distributed:?}");
        assert!(!distributed.is_empty());
    }

    #[test]
    fn shard_maps_are_validated() {
        let good = r#"{"version":2,"home":"a","shards":[{"name":"a","primary":"postgres://a/x"},{"name":"b","primary":"postgres://b/x","replicas":["postgres://b2/x"]}]}"#;
        let map = ShardMap::parse(good).unwrap();
        assert_eq!(map.home_index(), 0);
        assert_eq!(map.shards[1].replicas.len(), 1);
        for bad in [
            r#"{"version":1,"home":"a","shards":[]}"#,
            r#"{"version":1,"home":"z","shards":[{"name":"a","primary":"p"}]}"#,
            r#"{"version":1,"home":"a","shards":[{"name":"a","primary":"p"},{"name":"a","primary":"q"}]}"#,
            r#"{"version":1,"home":"a","shards":[{"name":"a","primary":"p","extra":1}]}"#,
        ] {
            assert!(ShardMap::parse(bad).is_err(), "{bad}");
        }
    }
}
