//! Guest data on the node's own storage (ADR 0021, ADR 0028, ADR 0036).
//!
//! A node without a separate guest data plane serves every creature's guest pairs and
//! confined documents from the `core.guest_pair` model, scoped by the creature: the
//! same operations and legacy semantics as a creature's own guest database, on any
//! storage provider.

use crate::auto::AutoCommit;
use aseman_domain::guest::{GuestKvOperation, GuestKvOutcome, LegacyKvNamespace};
use aseman_domain::{CreatureDatabaseBinding, CreatureId};
use aseman_ports::{GuestKv, PortError, PortResult};
use aseman_storage::client::core::guest_pair;
use aseman_storage::{FindMany, Mode, Models, Storage, StorageError, StorageResult, Trx};
use serde_json::{Map, Value};

/// Guest data served from the node's storage.
#[derive(Clone)]
pub struct StorageGuestKv(pub AutoCommit);

impl StorageGuestKv {
    #[must_use]
    pub fn new(storage: Storage) -> Self {
        Self(AutoCommit(storage))
    }

    /// Run `operation` for `creature`.
    ///
    /// # Errors
    ///
    /// An operation beyond the guest limits, a non-object `putJson`, or a storage
    /// failure.
    pub fn execute_for(
        &self,
        creature: CreatureId,
        operation: &GuestKvOperation,
    ) -> PortResult<GuestKvOutcome> {
        if !operation.is_valid() {
            return Err(PortError::Failed(
                "the operation exceeds the guest limits".to_owned(),
            ));
        }
        let owner = creature.to_string();
        self.0
            .run(Mode::ReadWrite, |trx| {
                Pairs { trx, owner: &owner }.execute(operation)
            })
            .map_err(|error| match error {
                StorageError::Conflict(_) => PortError::Conflict,
                other => PortError::Failed(other.to_string()),
            })
    }
}

impl GuestKv for StorageGuestKv {
    fn execute(
        &self,
        binding: &CreatureDatabaseBinding,
        operation: &GuestKvOperation,
    ) -> PortResult<GuestKvOutcome> {
        self.execute_for(binding.creature_id, operation)
    }
}

/// Store one guest pair of `creature` inside `trx` (the migration of a legacy guest
/// pair, ADR 0021).
///
/// # Errors
///
/// A storage failure.
pub fn put_pair(
    trx: &Trx,
    creature: CreatureId,
    namespace: LegacyKvNamespace,
    name: &str,
    value: &str,
) -> StorageResult<()> {
    let owner = creature.to_string();
    Pairs { trx, owner: &owner }.put(namespace, name, value)
}

/// The namespace a stored name denotes (`dbop`, `applet_db`, `json`).
#[must_use]
pub fn namespace(name: &str) -> Option<LegacyKvNamespace> {
    [
        LegacyKvNamespace::DbOp,
        LegacyKvNamespace::AppletDb,
        LegacyKvNamespace::Json,
    ]
    .into_iter()
    .find(|namespace| namespace.as_str() == name)
}

/// One creature's pairs inside a transaction.
struct Pairs<'a> {
    trx: &'a Trx,
    owner: &'a str,
}

impl Pairs<'_> {
    /// `{owner}::{namespace}::{name}`: the owner (a UUID) and the namespace never
    /// contain `::`, so the name is the unambiguous tail (PostgreSQL text holds no NUL).
    fn key(&self, namespace: LegacyKvNamespace, name: &str) -> String {
        [self.owner, "::", namespace.as_str(), "::", name].concat()
    }

    fn get(&self, namespace: LegacyKvNamespace, name: &str) -> StorageResult<Option<String>> {
        Ok(self
            .trx
            .guest_pair()
            .find_unique(guest_pair::by_key(self.key(namespace, name)))?
            .map(|pair| pair.value))
    }

    fn put(&self, namespace: LegacyKvNamespace, name: &str, value: &str) -> StorageResult<()> {
        let key = self.key(namespace, name);
        self.trx
            .guest_pair()
            .upsert(
                guest_pair::by_key(key.clone()),
                guest_pair::Create {
                    key,
                    owner_ref: self.owner.to_owned(),
                    namespace: namespace.as_str().to_owned(),
                    name: name.to_owned(),
                    value: value.to_owned(),
                },
                guest_pair::update().value(value),
            )
            .map(drop)
    }

    fn delete(&self, namespace: LegacyKvNamespace, name: &str) -> StorageResult<bool> {
        Ok(self
            .trx
            .guest_pair()
            .delete(guest_pair::by_key(self.key(namespace, name)))?
            .is_some())
    }

    /// Live pairs of `namespace` whose name starts with `prefix`, in byte order.
    fn list(
        &self,
        namespace: LegacyKvNamespace,
        prefix: &str,
        limit: Option<u32>,
    ) -> StorageResult<Vec<(String, String)>> {
        let mut query = FindMany::filter(
            guest_pair::owner_ref()
                .eq(self.owner)
                .and(guest_pair::namespace().eq(namespace.as_str()))
                .and(guest_pair::name().starts_with(prefix)),
        )
        .order_by(guest_pair::name().asc());
        if let Some(limit) = limit {
            query = query.take(u64::from(limit));
        }
        Ok(self
            .trx
            .guest_pair()
            .find_many(query)?
            .into_iter()
            .map(|pair| (pair.name, pair.value))
            .collect())
    }

    fn document(&self, record: &str) -> StorageResult<Option<Map<String, Value>>> {
        Ok(self.get(LegacyKvNamespace::Json, record)?.and_then(|text| {
            match serde_json::from_str(&text) {
                Ok(Value::Object(object)) => Some(object),
                _ => None,
            }
        }))
    }

    fn execute(&self, operation: &GuestKvOperation) -> StorageResult<GuestKvOutcome> {
        Ok(match operation {
            GuestKvOperation::Get { namespace, key } => GuestKvOutcome::Value {
                value: self.get(*namespace, key)?,
            },
            GuestKvOperation::Put {
                namespace,
                key,
                value,
            } => {
                self.put(*namespace, key, value)?;
                GuestKvOutcome::Written
            }
            GuestKvOperation::Delete { namespace, key } => GuestKvOutcome::Deleted {
                existed: self.delete(*namespace, key)?,
            },
            GuestKvOperation::List {
                namespace,
                prefix,
                limit,
            } => GuestKvOutcome::Listed {
                pairs: self.list(*namespace, prefix, Some(*limit))?,
            },
            GuestKvOperation::PutJson {
                key,
                path,
                data,
                merge,
            } => {
                let Ok(Value::Object(object)) = serde_json::from_str(data) else {
                    return Err(StorageError::invalid(
                        "putJson expects an object at the root",
                    ));
                };
                let prefix = [key.as_str(), "::"].concat();
                // Everything `index_json` can read lies at or below `path`.
                let snapshot = self
                    .list(
                        LegacyKvNamespace::Json,
                        &[prefix.as_str(), path].concat(),
                        None,
                    )?
                    .into_iter()
                    .filter_map(|(name, text)| match serde_json::from_str(&text) {
                        Ok(Value::Object(object)) => Some((name, object)),
                        _ => None,
                    })
                    .collect::<std::collections::BTreeMap<_, _>>();
                let recorded =
                    |record: &str| snapshot.get(&[prefix.as_str(), record].concat()).cloned();
                for (record, value) in
                    aseman_contracts::documents::json_index_writes(path, &object, *merge, &recorded)
                {
                    self.put(
                        LegacyKvNamespace::Json,
                        &[prefix.as_str(), record.as_str()].concat(),
                        &value,
                    )?;
                }
                GuestKvOutcome::Written
            }
            GuestKvOperation::GetJson { key, path } => GuestKvOutcome::Document {
                data: self
                    .document(&[key.as_str(), "::", path].concat())?
                    .map_or_else(
                        || "{}".to_owned(),
                        |object| Value::Object(object).to_string(),
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
                let mut records = self
                    .list(LegacyKvNamespace::Json, &below, None)?
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>();
                records.extend(exact);
                for record in records {
                    self.delete(LegacyKvNamespace::Json, &record)?;
                }
                GuestKvOutcome::Deleted { existed: true }
            }
            GuestKvOperation::ListJson { prefix, limit } => GuestKvOutcome::Keys {
                keys: self
                    .list(LegacyKvNamespace::Json, prefix, Some(*limit))?
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect(),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_domain::Uuid;

    fn kv() -> StorageGuestKv {
        StorageGuestKv::new(Storage::new(
            aseman_storage::memory::MemoryProvider::new(),
            aseman_storage::schema::Schema::catalog().unwrap(),
        ))
    }

    fn creature(byte: u8) -> CreatureId {
        CreatureId::from_uuid(Uuid::from_bytes([byte; 16]))
    }

    #[test]
    fn pairs_and_documents_stay_with_their_creature() {
        let kv = kv();
        let (alice, bob) = (creature(1), creature(2));
        let dbop = LegacyKvNamespace::DbOp;
        let put = |who, key: &str, value: &str| {
            kv.execute_for(
                who,
                &GuestKvOperation::Put {
                    namespace: dbop,
                    key: key.to_owned(),
                    value: value.to_owned(),
                },
            )
            .unwrap()
        };
        put(alice, "b", "2");
        put(alice, "a", "1");
        put(bob, "a", "other");
        assert_eq!(
            kv.execute_for(
                alice,
                &GuestKvOperation::List {
                    namespace: dbop,
                    prefix: String::new(),
                    limit: 10
                }
            )
            .unwrap(),
            GuestKvOutcome::Listed {
                pairs: vec![("a".into(), "1".into()), ("b".into(), "2".into())]
            }
        );
        assert_eq!(
            kv.execute_for(
                alice,
                &GuestKvOperation::Delete {
                    namespace: dbop,
                    key: "a".into()
                }
            )
            .unwrap(),
            GuestKvOutcome::Deleted { existed: true }
        );

        kv.execute_for(
            alice,
            &GuestKvOperation::PutJson {
                key: "counter".into(),
                path: "doc".into(),
                data: r#"{"n":1}"#.into(),
                merge: true,
            },
        )
        .unwrap();
        let get = |who| {
            kv.execute_for(
                who,
                &GuestKvOperation::GetJson {
                    key: "counter".into(),
                    path: "doc".into(),
                },
            )
            .unwrap()
        };
        assert_eq!(
            get(alice),
            GuestKvOutcome::Document {
                data: r#"{"n":1}"#.into()
            }
        );
        assert_eq!(get(bob), GuestKvOutcome::Document { data: "{}".into() });
        let listed = |who| {
            kv.execute_for(
                who,
                &GuestKvOperation::ListJson {
                    prefix: String::new(),
                    limit: 10,
                },
            )
            .unwrap()
        };
        assert_eq!(
            listed(alice),
            GuestKvOutcome::Keys {
                keys: vec!["counter::doc".into(), "counter::doc.n".into()]
            }
        );
        kv.execute_for(
            alice,
            &GuestKvOperation::DeleteJson {
                key: "counter".into(),
                path: String::new(),
            },
        )
        .unwrap();
        assert_eq!(listed(alice), GuestKvOutcome::Keys { keys: Vec::new() });
    }
}
