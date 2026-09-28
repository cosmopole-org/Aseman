use super::*;
use crate::RocksDbKvStore;
use aseman_contracts::capsule::{
    CapsuleDigest, CapsuleRelationship, DIGEST_ALGORITHM, ENCODING_VERSION, OwnerScope, QueryError,
    QueryErrorCode, QuerySort, StorageClass,
};
use aseman_storage_conformance::{StorageProviderHarness, query_error, reference_core_user_suite};

fn temp_store(label: &str) -> (std::path::PathBuf, Arc<dyn LegacyKvStore>) {
    let path = std::env::temp_dir().join(format!(
        "aseman-rocksdb-capsules-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let kv: Arc<dyn LegacyKvStore> = Arc::new(RocksDbKvStore::open_default(&path).unwrap());
    (path, kv)
}

fn store(label: &str, layout: CapsuleLayout) -> (std::path::PathBuf, RocksDbCapsuleStore) {
    let (path, kv) = temp_store(label);
    let store = RocksDbCapsuleStore::open(kv, false).unwrap();
    store.migrate_layout(layout).unwrap();
    (path, store)
}

fn error(error: CapsuleStoreError) -> QueryError {
    match error {
        CapsuleStoreError::Conflict => query_error(QueryErrorCode::RevisionConflict, "conflict"),
        CapsuleStoreError::Failed(message) if message.starts_with("invalid") => {
            query_error(QueryErrorCode::InvalidQuery, message)
        }
        CapsuleStoreError::Failed(message) if message.starts_with("unsupported") => {
            query_error(QueryErrorCode::UnsupportedCapability, message)
        }
        CapsuleStoreError::Failed(message) => query_error(QueryErrorCode::Unavailable, message),
    }
}

struct Harness<'a>(&'a RocksDbCapsuleStore);

impl StorageProviderHarness for Harness<'_> {
    fn reset(&mut self) -> Result<(), QueryError> {
        let writes = self
            .0
            .kv
            .scan_prefix(ROOT.as_bytes())
            .map_err(|error| query_error(QueryErrorCode::Unavailable, error.to_string()))?
            .into_iter()
            .filter(|(key, _)| key.as_slice() != LAYOUT_KEY.as_bytes())
            .map(|(key, _)| LegacyKvWrite::Delete { key })
            .collect::<Vec<_>>();
        self.0
            .kv
            .write_batch(&writes)
            .map_err(|error| query_error(QueryErrorCode::Unavailable, error.to_string()))
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.0.capabilities()
    }

    fn put(
        &mut self,
        capsule: CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> Result<(), QueryError> {
        CapsuleStore::put(self.0, &capsule, expected_revision).map_err(error)
    }

    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> Result<Option<CapsuleEnvelope>, QueryError> {
        CapsuleStore::get(self.0, kind, id).map_err(error)
    }

    fn query(&self, query: &CapsuleQuery) -> Result<Vec<CapsuleEnvelope>, QueryError> {
        CapsuleStore::query(self.0, query).map_err(error)
    }
}

fn user(id: u8, username: &str, created: i64) -> CapsuleEnvelope {
    CapsuleEnvelope {
        encoding_version: ENCODING_VERSION,
        id: CapsuleId([id; 16]),
        kind: CapsuleKind("core.user".to_owned()),
        storage_class: StorageClass::Core,
        owner_scope: OwnerScope::Global,
        schema_version: 1,
        revision: 1,
        created_at_micros: created,
        updated_at_micros: created,
        previous_integrity: None,
        integrity_hash: CapsuleDigest {
            algorithm: DIGEST_ALGORITHM.to_owned(),
            bytes: vec![0; 32],
        },
        tombstone: false,
        relationships: Vec::new(),
        body: Some(CapsuleValue::Object(BTreeMap::from([
            (
                "username".to_owned(),
                CapsuleValue::Text(username.to_owned()),
            ),
            ("public_key".to_owned(), CapsuleValue::Bytes(vec![id; 8])),
            ("status".to_owned(), CapsuleValue::Text("active".to_owned())),
            ("ratio".to_owned(), CapsuleValue::Float(f64::from(id) / 4.0)),
            ("note".to_owned(), CapsuleValue::Null),
        ]))),
    }
    .seal()
    .unwrap()
}

fn next(previous: &CapsuleEnvelope, body: Option<CapsuleValue>) -> CapsuleEnvelope {
    let mut capsule = previous.clone();
    capsule.revision += 1;
    capsule.updated_at_micros += 1;
    capsule.previous_integrity = Some(previous.integrity_hash.clone());
    capsule.tombstone = body.is_none();
    capsule.body = body;
    capsule.seal().unwrap()
}

#[test]
fn both_layouts_pass_the_storage_conformance_kit() {
    for layout in [CapsuleLayout::Flattened, CapsuleLayout::Capsule] {
        let (path, store) = store("conformance", layout);
        let report = reference_core_user_suite()
            .run(&mut Harness(&store))
            .unwrap_or_else(|failure| panic!("{layout:?}: {failure}"));
        assert_eq!(report.provider_id, "rocksdb-capsule-v1");
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[test]
fn flattened_capsules_keep_one_key_per_field_and_switch_layouts_losslessly() {
    let (path, store) = store("layout", CapsuleLayout::Flattened);
    let ada = user(1, "ada", 10);
    store.put(&ada, None).unwrap();
    let prefix = capsule_row_prefix("core.user", &ada.id);
    let fields = |store: &RocksDbCapsuleStore| {
        store
            .kv
            .scan_prefix(format!("{prefix}{FIELD}").as_bytes())
            .unwrap()
            .into_iter()
            .map(|(key, _)| {
                String::from_utf8(key).unwrap()[prefix.len() + FIELD.len()..].to_owned()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        fields(&store),
        ["note", "public_key", "ratio", "status", "username"]
    );
    assert_eq!(
        store
            .kv
            .get(format!("{prefix}{FIELD}username").as_bytes())
            .unwrap(),
        Some(encode_value(&CapsuleValue::Text("ada".to_owned())).unwrap())
    );

    // A revision that drops a field deletes that field's key.
    let mut body = BTreeMap::from([
        ("username".to_owned(), CapsuleValue::Text("ada".to_owned())),
        ("public_key".to_owned(), CapsuleValue::Bytes(vec![1; 8])),
        ("status".to_owned(), CapsuleValue::Text("away".to_owned())),
    ]);
    let second = next(&ada, Some(CapsuleValue::Object(body.clone())));
    store.put(&second, Some(1)).unwrap();
    assert_eq!(fields(&store), ["public_key", "status", "username"]);
    assert_eq!(
        store.get(&second.kind, &second.id).unwrap(),
        Some(second.clone())
    );

    // Capsule mode packs it into one key; flattening again restores the field keys.
    assert_eq!(store.migrate_layout(CapsuleLayout::Capsule).unwrap(), 1);
    assert!(fields(&store).is_empty());
    assert!(
        store
            .kv
            .get(packed_key("core.user", &ada.id).as_bytes())
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store.get(&second.kind, &second.id).unwrap(),
        Some(second.clone())
    );
    let reopened = RocksDbCapsuleStore::open(store.kv.clone(), false).unwrap();
    assert_eq!(reopened.layout(), CapsuleLayout::Capsule);
    body.insert("status".to_owned(), CapsuleValue::Text("back".to_owned()));
    let third = next(&second, Some(CapsuleValue::Object(body)));
    reopened.put(&third, Some(2)).unwrap();
    assert_eq!(
        reopened.migrate_layout(CapsuleLayout::Flattened).unwrap(),
        1
    );
    assert_eq!(fields(&reopened), ["public_key", "status", "username"]);
    assert_eq!(
        reopened.get(&third.kind, &third.id).unwrap(),
        Some(third.clone())
    );

    // A field edited outside the provider no longer matches the integrity hash.
    reopened
        .kv
        .write_batch(&[LegacyKvWrite::Put {
            key: format!("{prefix}{FIELD}status").into_bytes(),
            value: encode_value(&CapsuleValue::Text("forged".to_owned())).unwrap(),
        }])
        .unwrap();
    assert!(matches!(
        reopened.get(&third.kind, &third.id),
        Err(CapsuleStoreError::Failed(message)) if message.contains("integrity")
    ));
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn unique_indexes_hold_across_revisions_and_tombstones() {
    for layout in [CapsuleLayout::Flattened, CapsuleLayout::Capsule] {
        let (path, store) = store("unique", layout);
        let ada = user(1, "ada", 10);
        store.put(&ada, None).unwrap();
        // Another live user may not take the name, even inside one batch.
        assert_eq!(
            store.put(&user(2, "ada", 11), None),
            Err(CapsuleStoreError::Conflict)
        );
        assert_eq!(
            store.put_all(&[(user(3, "bob", 12), None), (user(4, "bob", 13), None)]),
            Err(CapsuleStoreError::Conflict)
        );
        assert_eq!(store.get(&ada.kind, &CapsuleId([3; 16])).unwrap(), None);
        // Renaming releases the old name; a tombstone releases every name.
        let mut renamed = ada.body.clone().unwrap();
        if let CapsuleValue::Object(body) = &mut renamed {
            body.insert(
                "username".to_owned(),
                CapsuleValue::Text("ada-l".to_owned()),
            );
        }
        let second = next(&ada, Some(renamed));
        store.put(&second, Some(1)).unwrap();
        let bob = user(2, "ada", 11);
        store.put(&bob, None).unwrap();
        assert_eq!(
            store.put(&user(5, "ada-l", 14), None),
            Err(CapsuleStoreError::Conflict)
        );
        store.put(&next(&second, None), Some(2)).unwrap();
        store.put(&user(5, "ada-l", 14), None).unwrap();
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[test]
fn queries_filter_sort_and_bound_like_sql() {
    let (path, store) = store("query", CapsuleLayout::Flattened);
    for (id, name) in [(1, "carol"), (2, "ada"), (3, "bob")] {
        store.put(&user(id, name, i64::from(id)), None).unwrap();
    }
    let mut member = user(9, "linked", 9);
    member.relationships = vec![CapsuleRelationship {
        name: "owner".to_owned(),
        target_kind: CapsuleKind("core.user".to_owned()),
        target_id: CapsuleId([1; 16]),
    }];
    let member = member.seal().unwrap();
    store.put(&member, None).unwrap();
    let names = |query: &CapsuleQuery| {
        store
            .query(query)
            .unwrap()
            .into_iter()
            .map(|capsule| match field(&capsule, "username") {
                CapsuleValue::Text(name) => name,
                other => panic!("{other:?}"),
            })
            .collect::<Vec<_>>()
    };
    let mut query = CapsuleQuery {
        kind: CapsuleKind("core.user".to_owned()),
        predicate: Some(QueryPredicate::Compare {
            field: "ratio".to_owned(),
            operator: ComparisonOperator::LessThan,
            value: CapsuleValue::Integer(1),
        }),
        projection: BTreeSet::new(),
        sort: vec![QuerySort {
            field: "username".to_owned(),
            direction: SortDirection::Descending,
        }],
        aggregates: Vec::new(),
        traversals: Vec::new(),
        limit: 2,
        cursor: None,
    };
    assert_eq!(names(&query), ["carol", "bob"]);
    query.limit = 10;
    // NOT over a NULL comparison is unknown, as in SQL: the missing email filters out.
    query.predicate = Some(QueryPredicate::Not {
        predicate: Box::new(QueryPredicate::Compare {
            field: "email".to_owned(),
            operator: ComparisonOperator::Equal,
            value: CapsuleValue::Text("x".to_owned()),
        }),
    });
    assert!(names(&query).is_empty());
    query.predicate = Some(QueryPredicate::RelationshipExists {
        relationship: "owner".to_owned(),
    });
    assert_eq!(names(&query), ["linked"]);
    query.predicate = Some(QueryPredicate::Compare {
        field: "note".to_owned(),
        operator: ComparisonOperator::Equal,
        value: CapsuleValue::Null,
    });
    assert_eq!(names(&query).len(), 4);
    query.limit = 0;
    assert!(store.query(&query).is_err());
    std::fs::remove_dir_all(path).unwrap();
}
