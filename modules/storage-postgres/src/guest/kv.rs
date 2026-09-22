//! The PostgreSQL [`GuestKv`] (A405, ADR 0021): legacy key/value operations on one
//! creature's reserved `_aseman_legacy_kv` table, inside that creature's role.
//!
//! Every row is a sealed `guest.legacy_kv` capsule. A write is the next revision of the
//! pair's capsule (migrated pairs keep their identity; new keys get a UUIDv7), and a
//! delete is its tombstone revision, so deletes really delete. Listing returns committed
//! pairs by exact prefix.

use super::{
    GUEST_SCHEMA, GuestPoolRouter, GuestPostgresError, LEGACY_KV_TABLE_SQL,
    ProvisionedGuestDatabase, database_error, legacy_kv_table_ddl,
};
use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleValue, OwnerScope, StorageClass,
};
use aseman_contracts::guest::{GuestBindingStatus, GuestDatabaseBinding};
use aseman_domain::guest::{GuestKvOperation, GuestKvOutcome, LegacyKvNamespace};
use aseman_domain::{BindingStatus, CreatureDatabaseBinding};
use aseman_ports::{GuestKv, PortError, PortResult};
use postgres::Transaction;
use std::collections::BTreeMap;

/// Legacy KV over provider-routed guest databases.
pub struct PostgresGuestKv {
    router: GuestPoolRouter,
}

impl PostgresGuestKv {
    #[must_use]
    pub fn new(router: GuestPoolRouter) -> Self {
        Self { router }
    }
}

fn port_error(error: GuestPostgresError) -> PortError {
    match error {
        GuestPostgresError::Inactive => {
            PortError::Failed("guest database is not active".to_owned())
        }
        other => PortError::Failed(other.to_string()),
    }
}

/// The provider binding for a catalog record. The provider re-derives and checks the
/// database and role names before any connection, so a tampered record cannot route.
fn provisioned(binding: &CreatureDatabaseBinding) -> ProvisionedGuestDatabase {
    ProvisionedGuestDatabase {
        binding: GuestDatabaseBinding {
            creature_id: *binding.creature_id.as_uuid().as_bytes(),
            provider_id: binding.provider_id.clone(),
            database_name: binding.database.clone(),
            role_name: binding.role.clone(),
            generation: binding.generation.get(),
            schema_catalog_revision: 0,
            status: match binding.status {
                BindingStatus::Active => GuestBindingStatus::Active,
                BindingStatus::Disabled => GuestBindingStatus::Disabled,
            },
        },
    }
}

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_micros()).unwrap_or(i64::MAX)
        })
}

/// The stored state of one pair.
struct Stored {
    id: uuid::Uuid,
    revision: i64,
    created_at_micros: i64,
    updated_at_micros: i64,
    integrity: Vec<u8>,
    tombstone: bool,
}

fn stored(
    transaction: &mut Transaction<'_>,
    namespace: LegacyKvNamespace,
    key: &str,
) -> Result<Option<Stored>, GuestPostgresError> {
    Ok(transaction
        .query_opt(
            &format!(
                "SELECT _aseman_id, _aseman_revision, _aseman_created_at_micros, \
                 _aseman_updated_at_micros, _aseman_integrity, _aseman_tombstone \
                 FROM {GUEST_SCHEMA}.{LEGACY_KV_TABLE_SQL} WHERE namespace = $1 AND key = $2 \
                 FOR UPDATE"
            ),
            &[&namespace.as_str(), &key],
        )
        .map_err(database_error)?
        .map(|row| Stored {
            id: row.get(0),
            revision: row.get(1),
            created_at_micros: row.get(2),
            updated_at_micros: row.get(3),
            integrity: row.get(4),
            tombstone: row.get(5),
        }))
}

/// Write the next revision of a pair: its value, or its tombstone when `value` is `None`.
fn write(
    transaction: &mut Transaction<'_>,
    creature: [u8; 16],
    namespace: LegacyKvNamespace,
    key: &str,
    value: Option<&str>,
    previous: Option<Stored>,
) -> Result<(), GuestPostgresError> {
    let now = now_micros();
    let (id, revision, created_at_micros, previous_integrity, updated_at_micros) = match &previous {
        Some(stored) => (
            stored.id,
            stored.revision + 1,
            stored.created_at_micros,
            Some(CapsuleDigest {
                algorithm: "sha2-256".to_owned(),
                bytes: stored.integrity.clone(),
            }),
            now.max(stored.updated_at_micros),
        ),
        None => (uuid::Uuid::now_v7(), 1, now, None, now),
    };
    let capsule = CapsuleEnvelope {
        encoding_version: 1,
        id: CapsuleId(*id.as_bytes()),
        kind: CapsuleKind("guest.legacy_kv".to_owned()),
        storage_class: StorageClass::GuestData,
        owner_scope: OwnerScope::Creature(creature),
        schema_version: 1,
        revision: u64::try_from(revision)
            .map_err(|_| GuestPostgresError::Invalid("revision overflow".to_owned()))?,
        created_at_micros,
        updated_at_micros,
        previous_integrity,
        integrity_hash: CapsuleDigest {
            algorithm: "sha2-256".to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: value.is_none(),
        relationships: Vec::new(),
        body: value.map(|value| {
            CapsuleValue::Object(BTreeMap::from([
                (
                    "namespace".to_owned(),
                    CapsuleValue::Text(namespace.as_str().to_owned()),
                ),
                ("key".to_owned(), CapsuleValue::Text(key.to_owned())),
                ("value".to_owned(), CapsuleValue::Text(value.to_owned())),
            ]))
        }),
    }
    .seal()
    .map_err(|error| GuestPostgresError::Invalid(error.to_string()))?;
    let canonical = capsule
        .canonical_bytes()
        .map_err(|error| GuestPostgresError::Invalid(error.to_string()))?;
    transaction
        .execute(
            &format!(
                "INSERT INTO {GUEST_SCHEMA}.{LEGACY_KV_TABLE_SQL} (_aseman_id, _aseman_revision, \
                 _aseman_created_at_micros, _aseman_updated_at_micros, _aseman_integrity, \
                 _aseman_tombstone, _aseman_capsule_cbor, namespace, key, value) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
                 ON CONFLICT (namespace, key) DO UPDATE SET \
                 _aseman_revision = EXCLUDED._aseman_revision, \
                 _aseman_updated_at_micros = EXCLUDED._aseman_updated_at_micros, \
                 _aseman_integrity = EXCLUDED._aseman_integrity, \
                 _aseman_tombstone = EXCLUDED._aseman_tombstone, \
                 _aseman_capsule_cbor = EXCLUDED._aseman_capsule_cbor, \
                 value = EXCLUDED.value"
            ),
            &[
                &id,
                &revision,
                &created_at_micros,
                &updated_at_micros,
                &capsule.integrity_hash.bytes,
                &capsule.tombstone,
                &canonical,
                &namespace.as_str(),
                &key,
                &value.unwrap_or_default(),
            ],
        )
        .map_err(database_error)?;
    Ok(())
}

/// The stored object of a live `json` row, if it holds one.
fn document_row(
    transaction: &mut Transaction<'_>,
    row: &str,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, GuestPostgresError> {
    let value: Option<String> = transaction
        .query_opt(
            &format!(
                "SELECT value FROM {GUEST_SCHEMA}.{LEGACY_KV_TABLE_SQL} \
                 WHERE namespace = 'json' AND key = $1 AND NOT _aseman_tombstone"
            ),
            &[&row],
        )
        .map_err(database_error)?
        .map(|found| found.get(0));
    Ok(value.and_then(|text| match serde_json::from_str(&text) {
        Ok(serde_json::Value::Object(object)) => Some(object),
        _ => None,
    }))
}

/// The live `json` rows with `prefix` that hold objects.
fn live_documents(
    transaction: &mut Transaction<'_>,
    prefix: &str,
) -> Result<BTreeMap<String, serde_json::Map<String, serde_json::Value>>, GuestPostgresError> {
    Ok(transaction
        .query(
            &format!(
                "SELECT key, value FROM {GUEST_SCHEMA}.{LEGACY_KV_TABLE_SQL} \
                 WHERE namespace = 'json' AND NOT _aseman_tombstone AND starts_with(key, $1)"
            ),
            &[&prefix],
        )
        .map_err(database_error)?
        .iter()
        .filter_map(|row| match serde_json::from_str(&row.get::<_, String>(1)) {
            Ok(serde_json::Value::Object(object)) => Some((row.get(0), object)),
            _ => None,
        })
        .collect())
}

/// Live `json` row keys with `prefix`, in byte order.
fn live_keys(
    transaction: &mut Transaction<'_>,
    prefix: &str,
    limit: i64,
) -> Result<Vec<String>, GuestPostgresError> {
    Ok(transaction
        .query(
            &format!(
                "SELECT key FROM {GUEST_SCHEMA}.{LEGACY_KV_TABLE_SQL} \
                 WHERE namespace = 'json' AND NOT _aseman_tombstone AND starts_with(key, $1) \
                 ORDER BY key COLLATE \"C\" LIMIT $2"
            ),
            &[&prefix, &limit],
        )
        .map_err(database_error)?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

impl GuestKv for PostgresGuestKv {
    fn execute(
        &self,
        binding: &CreatureDatabaseBinding,
        operation: &GuestKvOperation,
    ) -> PortResult<GuestKvOutcome> {
        if !operation.is_valid() {
            return Err(PortError::Failed(
                "the operation exceeds the guest limits".to_owned(),
            ));
        }
        let database = provisioned(binding);
        let creature = database.binding.creature_id;
        self.router
            .with_transaction(&database, |transaction| {
                transaction
                    .batch_execute(&legacy_kv_table_ddl())
                    .map_err(database_error)?;
                Ok(match operation {
                    GuestKvOperation::Get { namespace, key } => GuestKvOutcome::Value {
                        value: transaction
                            .query_opt(
                                &format!(
                                    "SELECT value FROM {GUEST_SCHEMA}.{LEGACY_KV_TABLE_SQL} \
                                     WHERE namespace = $1 AND key = $2 AND NOT _aseman_tombstone"
                                ),
                                &[&namespace.as_str(), key],
                            )
                            .map_err(database_error)?
                            .map(|row| row.get(0)),
                    },
                    GuestKvOperation::Put {
                        namespace,
                        key,
                        value,
                    } => {
                        let previous = stored(transaction, *namespace, key)?;
                        write(
                            transaction,
                            creature,
                            *namespace,
                            key,
                            Some(value),
                            previous,
                        )?;
                        GuestKvOutcome::Written
                    }
                    GuestKvOperation::Delete { namespace, key } => {
                        match stored(transaction, *namespace, key)? {
                            Some(previous) if !previous.tombstone => {
                                write(
                                    transaction,
                                    creature,
                                    *namespace,
                                    key,
                                    None,
                                    Some(previous),
                                )?;
                                GuestKvOutcome::Deleted { existed: true }
                            }
                            _ => GuestKvOutcome::Deleted { existed: false },
                        }
                    }
                    GuestKvOperation::PutJson {
                        key,
                        path,
                        data,
                        merge,
                    } => {
                        let Ok(serde_json::Value::Object(object)) = serde_json::from_str(data)
                        else {
                            return Err(GuestPostgresError::Invalid(
                                "putJson expects an object at the root".to_owned(),
                            ));
                        };
                        let prefix = [key.as_str(), "::"].concat();
                        // Everything `index_json` can read lies at or below `path`.
                        let snapshot =
                            live_documents(transaction, &[prefix.as_str(), path].concat())?;
                        let recorded = |record: &str| {
                            snapshot.get(&[prefix.as_str(), record].concat()).cloned()
                        };
                        let writes = aseman_contracts::legacy_documents::legacy_json_index_writes(
                            path, &object, *merge, &recorded,
                        );
                        for (record, value) in writes {
                            let row = [prefix.as_str(), record.as_str()].concat();
                            let previous = stored(transaction, LegacyKvNamespace::Json, &row)?;
                            write(
                                transaction,
                                creature,
                                LegacyKvNamespace::Json,
                                &row,
                                Some(&value),
                                previous,
                            )?;
                        }
                        GuestKvOutcome::Written
                    }
                    GuestKvOperation::GetJson { key, path } => GuestKvOutcome::Document {
                        data: document_row(transaction, &[key.as_str(), "::", path].concat())?
                            .map_or_else(
                                || "{}".to_owned(),
                                |object| serde_json::Value::Object(object).to_string(),
                            ),
                    },
                    GuestKvOperation::DeleteJson { key, path } => {
                        let (exact, below) = if path.is_empty() {
                            (None, [key.as_str(), "::"].concat())
                        } else {
                            let record = [key.as_str(), "::", path].concat();
                            let below = [record.as_str(), "."].concat();
                            (Some(record), below)
                        };
                        let mut rows = live_keys(transaction, &below, i64::MAX)?;
                        rows.extend(exact);
                        for row in rows {
                            if let Some(previous) =
                                stored(transaction, LegacyKvNamespace::Json, &row)?
                                    .filter(|previous| !previous.tombstone)
                            {
                                write(
                                    transaction,
                                    creature,
                                    LegacyKvNamespace::Json,
                                    &row,
                                    None,
                                    Some(previous),
                                )?;
                            }
                        }
                        GuestKvOutcome::Deleted { existed: true }
                    }
                    GuestKvOperation::ListJson { prefix, limit } => GuestKvOutcome::Keys {
                        keys: live_keys(transaction, prefix, i64::from(*limit))?,
                    },
                    GuestKvOperation::List {
                        namespace,
                        prefix,
                        limit,
                    } => GuestKvOutcome::Listed {
                        pairs: transaction
                            .query(
                                &format!(
                                    "SELECT key, value FROM {GUEST_SCHEMA}.{LEGACY_KV_TABLE_SQL} \
                                     WHERE namespace = $1 AND NOT _aseman_tombstone \
                                       AND starts_with(key, $2) \
                                     ORDER BY key COLLATE \"C\" LIMIT $3"
                                ),
                                &[&namespace.as_str(), prefix, &i64::from(*limit)],
                            )
                            .map_err(database_error)?
                            .iter()
                            .map(|row| (row.get(0), row.get(1)))
                            .collect(),
                    },
                })
            })
            .map_err(port_error)
    }
}
