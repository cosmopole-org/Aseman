//! Reusable behavioral conformance checks for capsule storage providers.
#![forbid(unsafe_code)]

use aseman_contracts::capsule::{
    CapsuleDefinition, CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery, ProviderCapabilities,
    QueryError, QueryErrorCode, StorageCapability, decode_canonical_value, negotiate_capabilities,
};

pub const SUITE_VERSION: &str = "storage-v1";

pub trait StorageProviderHarness {
    fn reset(&mut self) -> Result<(), QueryError>;
    fn capabilities(&self) -> aseman_contracts::capsule::ProviderCapabilities;
    fn put(
        &mut self,
        capsule: CapsuleEnvelope,
        expected_revision: Option<u64>,
    ) -> Result<(), QueryError>;
    fn get(
        &self,
        kind: &CapsuleKind,
        id: &CapsuleId,
    ) -> Result<Option<CapsuleEnvelope>, QueryError>;
    fn query(&self, query: &CapsuleQuery) -> Result<Vec<CapsuleEnvelope>, QueryError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StorageConformanceReport {
    pub suite_version: String,
    pub provider_id: String,
    pub passed_cases: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct StorageConformanceKit {
    pub definition: CapsuleDefinition,
    pub initial: CapsuleEnvelope,
    pub revision_two: CapsuleEnvelope,
    pub tombstone: CapsuleEnvelope,
    pub query: CapsuleQuery,
}

impl StorageConformanceKit {
    pub fn validate_vectors() -> Result<(), String> {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../contracts/capsule/fixtures/canonical-v1.json"
        )))
        .map_err(|error| error.to_string())?;
        let bytes = hex::decode(
            fixture["canonical_hex"]
                .as_str()
                .ok_or_else(|| "canonical fixture omits canonical_hex".to_owned())?,
        )
        .map_err(|error| error.to_string())?;
        let capsule =
            CapsuleEnvelope::from_canonical_bytes(&bytes).map_err(|error| error.to_string())?;
        let expected_integrity = fixture["integrity_hex"]
            .as_str()
            .ok_or_else(|| "canonical fixture omits integrity_hex".to_owned())?;
        if hex::encode(capsule.integrity_hash.bytes) != expected_integrity {
            return Err("canonical fixture integrity does not match its bytes".to_owned());
        }

        let invalid: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../contracts/capsule/fixtures/invalid-cbor-v1.json"
        )))
        .map_err(|error| error.to_string())?;
        for vector in invalid
            .as_array()
            .ok_or_else(|| "invalid-CBOR fixture must be an array".to_owned())?
        {
            let name = vector["name"].as_str().unwrap_or("unnamed");
            let source = vector["hex"]
                .as_str()
                .ok_or_else(|| format!("invalid vector {name} omits hex"))?;
            let bytes = hex::decode(source).map_err(|error| error.to_string())?;
            if decode_canonical_value(&bytes).is_ok() {
                return Err(format!("invalid vector {name} was accepted"));
            }
        }

        let _: ProviderCapabilities = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../contracts/capsule/fixtures/compatible-core-capabilities.json"
        )))
        .map_err(|error| error.to_string())?;
        let _: ProviderCapabilities = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../contracts/capsule/fixtures/incompatible-eventual-capabilities.json"
        )))
        .map_err(|error| error.to_string())?;
        let _: CapsuleQuery = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../contracts/capsule/query/fixtures/valid-bounded-query.json"
        )))
        .map_err(|error| error.to_string())?;
        let invalid_query = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../contracts/capsule/query/fixtures/invalid-raw-provider-query.json"
        ));
        if serde_json::from_str::<CapsuleQuery>(invalid_query).is_ok() {
            return Err("raw provider query fixture was accepted".to_owned());
        }
        let errors: Vec<QueryError> = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../contracts/capsule/query/fixtures/errors.json"
        )))
        .map_err(|error| error.to_string())?;
        if errors.is_empty() {
            return Err("query error fixture is empty".to_owned());
        }
        Ok(())
    }

    pub fn run(
        &self,
        provider: &mut impl StorageProviderHarness,
    ) -> Result<StorageConformanceReport, String> {
        Self::validate_vectors()?;
        self.definition
            .validate()
            .map_err(|error| error.to_string())?;
        self.query
            .validate(&self.definition)
            .map_err(|error| error.to_string())?;
        for capsule in [&self.initial, &self.revision_two, &self.tombstone] {
            capsule.verify().map_err(|error| error.to_string())?;
            if capsule.kind != self.definition.kind || capsule.id != self.initial.id {
                return Err("conformance revisions do not share the declared identity".to_owned());
            }
        }
        if self.initial.revision != 1
            || self.revision_two.revision != 2
            || self.tombstone.revision != 3
            || self.revision_two.previous_integrity.as_ref() != Some(&self.initial.integrity_hash)
            || self.tombstone.previous_integrity.as_ref() != Some(&self.revision_two.integrity_hash)
            || !self.tombstone.tombstone
        {
            return Err("conformance revision chain is invalid".to_owned());
        }

        provider.reset().map_err(render_query_error)?;
        let capabilities = provider.capabilities();
        let report = negotiate_capabilities(&self.definition, &capabilities)
            .map_err(|error| error.to_string())?;
        if !report.compatible {
            return Err(format!(
                "provider omits required capabilities: {:?}",
                report.missing
            ));
        }

        let mut tampered = self.initial.clone();
        tampered.updated_at_micros += 1;
        require_error_code(
            provider.put(tampered, None),
            QueryErrorCode::InvalidQuery,
            "tampered capsule",
        )?;

        provider
            .put(self.initial.clone(), None)
            .map_err(render_query_error)?;
        require_exact(provider, &self.initial)?;
        provider
            .put(self.initial.clone(), None)
            .map_err(render_query_error)?;
        require_error_code(
            provider.put(self.revision_two.clone(), Some(0)),
            QueryErrorCode::RevisionConflict,
            "revision conflict",
        )?;
        provider
            .put(self.revision_two.clone(), Some(1))
            .map_err(render_query_error)?;
        require_exact(provider, &self.revision_two)?;
        provider
            .put(self.revision_two.clone(), Some(1))
            .map_err(render_query_error)?;

        let rows = provider.query(&self.query).map_err(render_query_error)?;
        if rows.len() != 1 || rows[0] != self.revision_two {
            return Err("bounded query did not return the current exact revision".to_owned());
        }

        provider
            .put(self.tombstone.clone(), Some(2))
            .map_err(render_query_error)?;
        require_exact(provider, &self.tombstone)?;

        Ok(StorageConformanceReport {
            suite_version: SUITE_VERSION.to_owned(),
            provider_id: capabilities.provider_id,
            passed_cases: vec![
                "encoding.canonical-v1".to_owned(),
                "capabilities.exact".to_owned(),
                "integrity.reject-tamper".to_owned(),
                "revision.compare-and-set".to_owned(),
                "idempotency.canonical-retry".to_owned(),
                "query.typed-bounded".to_owned(),
                "tombstone.revision-chain".to_owned(),
            ],
        })
    }
}

fn require_exact(
    provider: &impl StorageProviderHarness,
    expected: &CapsuleEnvelope,
) -> Result<(), String> {
    let actual = provider
        .get(&expected.kind, &expected.id)
        .map_err(render_query_error)?
        .ok_or_else(|| "provider lost a stored capsule".to_owned())?;
    let actual_bytes = actual
        .canonical_bytes()
        .map_err(|error| error.to_string())?;
    let expected_bytes = expected
        .canonical_bytes()
        .map_err(|error| error.to_string())?;
    if actual_bytes != expected_bytes {
        return Err("provider changed canonical capsule content".to_owned());
    }
    Ok(())
}

fn require_error_code<T>(
    result: Result<T, QueryError>,
    expected: QueryErrorCode,
    case: &str,
) -> Result<(), String> {
    match result {
        Err(error) if error.code == expected => Ok(()),
        Err(error) => Err(format!(
            "{case} returned {:?}, expected {expected:?}",
            error.code
        )),
        Ok(_) => Err(format!("{case} unexpectedly succeeded")),
    }
}

fn render_query_error(error: QueryError) -> String {
    format!("{:?}: {}", error.code, error.message)
}

pub fn query_error(code: QueryErrorCode, message: impl Into<String>) -> QueryError {
    QueryError {
        retryable: matches!(code, QueryErrorCode::Unavailable),
        code,
        message: message.into(),
        missing_capabilities: std::collections::BTreeSet::<StorageCapability>::new(),
    }
}

/// Provider-neutral core-user revisions used by database integration suites.
#[must_use]
pub fn reference_core_user_suite() -> StorageConformanceKit {
    use aseman_contracts::capsule::{
        CapsuleDigest, CapsuleValue, ConsistencyProfile, DIGEST_ALGORITHM, ENCODING_VERSION,
        FieldDefinition, FieldType, IndexDefinition, OwnerScope, StorageClass,
    };
    use std::collections::{BTreeMap, BTreeSet};

    let kind = CapsuleKind("core.user".to_owned());
    let body = |username: &str| {
        CapsuleValue::Object(BTreeMap::from([
            (
                "username".to_owned(),
                CapsuleValue::Text(username.to_owned()),
            ),
            (
                "email".to_owned(),
                CapsuleValue::Text(format!("{username}@example.invalid")),
            ),
            ("public_key".to_owned(), CapsuleValue::Bytes(vec![7; 32])),
            ("status".to_owned(), CapsuleValue::Text("active".to_owned())),
        ]))
    };
    let placeholder = || CapsuleDigest {
        algorithm: DIGEST_ALGORITHM.to_owned(),
        bytes: vec![0; 32],
    };
    let initial = CapsuleEnvelope {
        encoding_version: ENCODING_VERSION,
        id: CapsuleId([9; 16]),
        kind: kind.clone(),
        storage_class: StorageClass::Core,
        owner_scope: OwnerScope::Global,
        schema_version: 1,
        revision: 1,
        created_at_micros: 1_700_000_000_000_000,
        updated_at_micros: 1_700_000_000_000_000,
        previous_integrity: None,
        integrity_hash: placeholder(),
        tombstone: false,
        relationships: Vec::new(),
        body: Some(body("conformance-user")),
    }
    .seal()
    .expect("reference capsule is valid");
    let mut revision_two = initial.clone();
    revision_two.revision = 2;
    revision_two.updated_at_micros += 1;
    revision_two.previous_integrity = Some(initial.integrity_hash.clone());
    revision_two.integrity_hash = placeholder();
    revision_two.body = Some(body("conformance-user-v2"));
    revision_two = revision_two.seal().expect("reference revision is valid");
    let mut tombstone = revision_two.clone();
    tombstone.revision = 3;
    tombstone.updated_at_micros += 1;
    tombstone.previous_integrity = Some(revision_two.integrity_hash.clone());
    tombstone.integrity_hash = placeholder();
    tombstone.tombstone = true;
    tombstone.body = None;
    tombstone = tombstone.seal().expect("reference tombstone is valid");

    let fields = BTreeMap::from([
        (
            "username".to_owned(),
            FieldDefinition {
                field_type: FieldType::Text,
                searchable: true,
                sortable: true,
                nullable: false,
            },
        ),
        (
            "email".to_owned(),
            FieldDefinition {
                field_type: FieldType::Text,
                searchable: true,
                sortable: true,
                nullable: true,
            },
        ),
        (
            "public_key".to_owned(),
            FieldDefinition {
                field_type: FieldType::Bytes,
                searchable: true,
                sortable: false,
                nullable: false,
            },
        ),
        (
            "status".to_owned(),
            FieldDefinition {
                field_type: FieldType::Text,
                searchable: true,
                sortable: true,
                nullable: false,
            },
        ),
    ]);
    StorageConformanceKit {
        definition: CapsuleDefinition {
            kind: kind.clone(),
            schema_version: 1,
            storage_class: StorageClass::Core,
            consistency: ConsistencyProfile::Serializable,
            required_fields: BTreeSet::from([
                "username".to_owned(),
                "public_key".to_owned(),
                "status".to_owned(),
            ]),
            fields,
            indexes: vec![IndexDefinition {
                name: "username".to_owned(),
                fields: vec!["username".to_owned()],
                unique: true,
            }],
            relationships: BTreeMap::new(),
        },
        initial,
        revision_two,
        tombstone,
        query: CapsuleQuery {
            kind,
            predicate: None,
            projection: BTreeSet::from(["username".to_owned()]),
            sort: Vec::new(),
            aggregates: Vec::new(),
            traversals: Vec::new(),
            limit: 100,
            cursor: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_contracts::capsule::{
        CapsuleDigest, CapsuleRelationship, CapsuleValue, ConsistencyProfile, DIGEST_ALGORITHM,
        ENCODING_VERSION, FieldDefinition, FieldType, OwnerScope, ProviderCapabilities,
        RelationshipDefinition, StorageClass,
    };
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Default)]
    struct MemoryProvider {
        values: BTreeMap<(CapsuleKind, CapsuleId), CapsuleEnvelope>,
    }

    impl StorageProviderHarness for MemoryProvider {
        fn reset(&mut self) -> Result<(), QueryError> {
            self.values.clear();
            Ok(())
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                provider_id: "conformance-memory".to_owned(),
                capabilities: BTreeSet::from([
                    StorageCapability::TransactionsSingleCapsule,
                    StorageCapability::ConsistencyLinearizable,
                    StorageCapability::RelationshipsForeignKeys,
                ]),
                max_query_limit: 10_000,
                max_transaction_capsules: 1,
            }
        }

        fn put(
            &mut self,
            capsule: CapsuleEnvelope,
            expected_revision: Option<u64>,
        ) -> Result<(), QueryError> {
            capsule
                .verify()
                .map_err(|error| query_error(QueryErrorCode::InvalidQuery, error.to_string()))?;
            let key = (capsule.kind.clone(), capsule.id.clone());
            let current_revision = self.values.get(&key).map(|value| value.revision);
            if self.values.get(&key) == Some(&capsule) {
                return Ok(());
            }
            let valid = match (current_revision, expected_revision) {
                (None, None) => capsule.revision == 1,
                (Some(current), Some(expected)) => {
                    current == expected && capsule.revision == current + 1
                }
                _ => false,
            };
            if !valid {
                return Err(query_error(
                    QueryErrorCode::RevisionConflict,
                    "compare-and-set precondition failed",
                ));
            }
            self.values.insert(key, capsule);
            Ok(())
        }

        fn get(
            &self,
            kind: &CapsuleKind,
            id: &CapsuleId,
        ) -> Result<Option<CapsuleEnvelope>, QueryError> {
            Ok(self.values.get(&(kind.clone(), id.clone())).cloned())
        }

        fn query(&self, query: &CapsuleQuery) -> Result<Vec<CapsuleEnvelope>, QueryError> {
            let limit = usize::try_from(query.limit).unwrap_or(usize::MAX);
            Ok(self
                .values
                .values()
                .filter(|capsule| capsule.kind == query.kind)
                .take(limit)
                .cloned()
                .collect())
        }
    }

    fn suite() -> StorageConformanceKit {
        let creature_kind = CapsuleKind("core.creature".to_owned());
        let program_kind = CapsuleKind("core.program".to_owned());
        let initial = CapsuleEnvelope {
            encoding_version: ENCODING_VERSION,
            id: CapsuleId([1; 16]),
            kind: program_kind.clone(),
            storage_class: StorageClass::Core,
            owner_scope: OwnerScope::Creature([2; 16]),
            schema_version: 1,
            revision: 1,
            created_at_micros: 1_700_000_000_000_000,
            updated_at_micros: 1_700_000_000_000_000,
            previous_integrity: None,
            integrity_hash: placeholder_digest(),
            tombstone: false,
            relationships: vec![CapsuleRelationship {
                name: "creature".to_owned(),
                target_kind: creature_kind.clone(),
                target_id: CapsuleId([2; 16]),
            }],
            body: Some(CapsuleValue::Object(BTreeMap::from([
                ("name".to_owned(), CapsuleValue::Text("demo".to_owned())),
                ("weight".to_owned(), CapsuleValue::Float(1.5)),
            ]))),
        }
        .seal()
        .unwrap();
        let mut revision_two = initial.clone();
        revision_two.revision = 2;
        revision_two.updated_at_micros += 1;
        revision_two.previous_integrity = Some(initial.integrity_hash.clone());
        revision_two.body = Some(CapsuleValue::Object(BTreeMap::from([
            ("name".to_owned(), CapsuleValue::Text("demo-v2".to_owned())),
            ("weight".to_owned(), CapsuleValue::Float(2.0)),
        ])));
        revision_two = revision_two.seal().unwrap();
        let mut tombstone = revision_two.clone();
        tombstone.revision = 3;
        tombstone.updated_at_micros += 1;
        tombstone.previous_integrity = Some(revision_two.integrity_hash.clone());
        tombstone.tombstone = true;
        tombstone.body = None;
        tombstone = tombstone.seal().unwrap();

        let fields = BTreeMap::from([
            (
                "name".to_owned(),
                FieldDefinition {
                    field_type: FieldType::Text,
                    searchable: true,
                    sortable: true,
                    nullable: false,
                },
            ),
            (
                "weight".to_owned(),
                FieldDefinition {
                    field_type: FieldType::Float,
                    searchable: true,
                    sortable: true,
                    nullable: false,
                },
            ),
        ]);
        StorageConformanceKit {
            definition: CapsuleDefinition {
                kind: program_kind.clone(),
                schema_version: 1,
                storage_class: StorageClass::Core,
                consistency: ConsistencyProfile::Serializable,
                required_fields: BTreeSet::from(["name".to_owned(), "weight".to_owned()]),
                fields,
                indexes: Vec::new(),
                relationships: BTreeMap::from([(
                    "creature".to_owned(),
                    RelationshipDefinition {
                        target_kind: creature_kind,
                        required: true,
                    },
                )]),
            },
            initial,
            revision_two,
            tombstone,
            query: CapsuleQuery {
                kind: program_kind,
                predicate: None,
                projection: BTreeSet::from(["name".to_owned()]),
                sort: Vec::new(),
                aggregates: Vec::new(),
                traversals: Vec::new(),
                limit: 100,
                cursor: None,
            },
        }
    }

    fn placeholder_digest() -> CapsuleDigest {
        CapsuleDigest {
            algorithm: DIGEST_ALGORITHM.to_owned(),
            bytes: vec![0; 32],
        }
    }

    #[test]
    fn canonical_vectors_are_consumable_by_the_shared_kit() {
        StorageConformanceKit::validate_vectors().unwrap();
    }

    #[test]
    fn memory_reference_harness_passes_all_behavioral_cases() {
        let report = suite().run(&mut MemoryProvider::default()).unwrap();
        assert_eq!(report.suite_version, SUITE_VERSION);
        assert_eq!(report.passed_cases.len(), 7);
    }

    #[test]
    fn missing_required_capability_fails_closed() {
        let mut provider = MemoryProvider::default();
        let mut test = suite();
        test.definition.storage_class = StorageClass::GuestData;
        let error = test.run(&mut provider).unwrap_err();
        assert!(error.contains("GuestDatabaseIsolatedRoles"));
    }
}
