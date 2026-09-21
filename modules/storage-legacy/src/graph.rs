//! Two-pass legacy snapshot graph: assembly, owner resolution, and reviewed dispatch.

use super::*;

impl LegacySnapshotGraph {
    pub fn assemble(records: Vec<LegacyPhysicalRecord>) -> LegacyMigrationResult<Self> {
        let mut graph = Self::default();
        for record in records {
            let key = String::from_utf8(record.key).map_err(|_| {
                LegacyMigrationError::Invalid(
                    "legacy application RocksDB contains a non-UTF-8 key".to_owned(),
                )
            })?;
            if let Some(rest) = key.strip_prefix("obj::") {
                let (family, object_and_column) = rest.split_once("::").ok_or_else(|| {
                    LegacyMigrationError::Invalid(format!("malformed legacy object key {key}"))
                })?;
                let (object_id, column) = object_and_column.rsplit_once("::").ok_or_else(|| {
                    LegacyMigrationError::Invalid(format!("malformed legacy object key {key}"))
                })?;
                if family.is_empty() || object_id.is_empty() || column.is_empty() {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "malformed legacy object key {key}"
                    )));
                }
                let columns = graph
                    .objects
                    .entry((family.to_owned(), object_id.to_owned()))
                    .or_default();
                if columns.insert(column.to_owned(), record.value).is_some() {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "duplicate legacy object column {key}"
                    )));
                }
            } else if let Some(link) = key.strip_prefix("link::") {
                if graph.links.insert(link.to_owned(), record.value).is_some() {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "duplicate legacy link {key}"
                    )));
                }
            } else if let Some(document) = key.strip_prefix("json::") {
                // Legacy keys may contain `::`; dotted paths never do.
                let (document_key, path) = document.rsplit_once("::").ok_or_else(|| {
                    LegacyMigrationError::Invalid(format!("malformed legacy JSON key {key}"))
                })?;
                if document_key.is_empty() || path.is_empty() {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "malformed legacy JSON key {key}"
                    )));
                }
                if graph
                    .documents
                    .entry(document_key.to_owned())
                    .or_default()
                    .insert(path.to_owned(), record.value)
                    .is_some()
                {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "duplicate legacy JSON record {key}"
                    )));
                }
            } else if let Some(index) = key.strip_prefix("index::") {
                if graph
                    .indexes
                    .insert(index.to_owned(), record.value)
                    .is_some()
                {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "duplicate legacy index {key}"
                    )));
                }
            } else if is_reviewed_operational_raw_key(&key) {
                graph.raw.insert(key, record.value);
            } else if key.starts_with("god::") {
                // ADR 0020: a hand-written superuser flag needs administrator review.
                return Err(LegacyMigrationError::Unmapped {
                    family: "raw.god".to_owned(),
                    key,
                });
            } else {
                return Err(LegacyMigrationError::Unmapped {
                    family: record.family,
                    key,
                });
            }
        }
        Ok(graph)
    }

    /// Transform every currently reviewed typed family in deterministic order.
    /// Presence of an unreviewed typed family is a hard error.
    pub fn transform_reviewed(
        &self,
        migration_time_micros: i64,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        self.transform_reviewed_with_evidence(
            migration_time_micros,
            &LegacyTransformEvidence::default(),
        )
    }

    pub fn transform_reviewed_with_file_artifacts(
        &self,
        migration_time_micros: i64,
        file_artifacts: &BTreeMap<String, LegacyFileArtifactEvidence>,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        self.transform_reviewed_with_evidence(
            migration_time_micros,
            &LegacyTransformEvidence {
                file_artifacts: file_artifacts.clone(),
                ..LegacyTransformEvidence::default()
            },
        )
    }

    pub fn transform_reviewed_with_evidence(
        &self,
        migration_time_micros: i64,
        evidence: &LegacyTransformEvidence,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        self.verify_links_and_indexes()?;
        // ADR 0019/0020: verified for consistency, never exported.
        self.verify_legacy_custodial_keys()?;
        self.verify_legacy_operational_keys(&evidence.local_origins)?;
        // ADR 0022: observed runtime stays with the VMM; control links are verified.
        self.legacy_vmm_handoff_inventory()?;
        self.verify_legacy_vm_control_links()?;
        let mut capsules = Vec::new();
        for ((family, legacy_id), columns) in &self.objects {
            let transformed = match family.as_str() {
                "Program" => vec![transform_legacy_program(
                    legacy_id,
                    columns,
                    migration_time_micros,
                )?],
                "Entity" => {
                    let program_id = required_utf8_column("Entity", columns, "programId")?;
                    let owner = self.resolve_program_creature(&program_id)?;
                    vec![transform_legacy_entity(
                        legacy_id,
                        columns,
                        &owner,
                        migration_time_micros,
                    )?]
                }
                "Store" => {
                    let creator = self.resolve_store_creator(legacy_id)?;
                    vec![transform_legacy_store(
                        legacy_id,
                        columns,
                        &creator,
                        migration_time_micros,
                    )?]
                }
                "Chain" => {
                    let store_id = required_utf8_column("Chain", columns, "storeId")?;
                    let creator = self.resolve_store_creator(&store_id)?;
                    vec![transform_legacy_chain(
                        legacy_id,
                        columns,
                        &creator,
                        migration_time_micros,
                    )?]
                }
                "ChainShard" => {
                    let chain_id = required_utf8_column("ChainShard", columns, "workChainId")?;
                    let chain = self.object("Chain", &chain_id)?;
                    let store_id = required_utf8_column("Chain", chain, "storeId")?;
                    let creator = self.resolve_store_creator(&store_id)?;
                    vec![transform_legacy_chain_shard(
                        legacy_id,
                        columns,
                        &creator,
                        migration_time_micros,
                    )?]
                }
                "Session" => {
                    let subject = required_utf8_column("Session", columns, "userId")?;
                    let user = self.resolve_user_for_creature(&subject)?;
                    vec![transform_legacy_session_revocation(
                        legacy_id,
                        columns,
                        &user,
                        migration_time_micros,
                    )?]
                }
                "File" => {
                    let artifact = evidence.file_artifacts.get(legacy_id).ok_or_else(|| {
                        LegacyMigrationError::Unmapped {
                            family: "File.artifact".to_owned(),
                            key: legacy_id.clone(),
                        }
                    })?;
                    let subject = required_utf8_column("File", columns, "ownerId")?;
                    let user = self.resolve_user_for_creature(&subject)?;
                    vec![transform_legacy_file(
                        legacy_id,
                        columns,
                        artifact,
                        &user,
                        migration_time_micros,
                    )?]
                }
                "Creature" => {
                    let finance = evidence.finance.as_ref().ok_or_else(|| {
                        LegacyMigrationError::Unmapped {
                            family: "Creature.finance_config".to_owned(),
                            key: legacy_id.clone(),
                        }
                    })?;
                    let email = self.optional_link_utf8(&format!("UserIdToEmail::{legacy_id}"))?;
                    let creature_type = required_utf8_column("Creature", columns, "type")?;
                    let owner_user_id = if creature_type == "human" {
                        legacy_id.clone()
                    } else {
                        let owner = required_utf8_column("Creature", columns, "ownerId")?;
                        let owner_columns = self.object("Creature", &owner)?;
                        if required_utf8_column("Creature", owner_columns, "type")? != "human" {
                            return Err(LegacyMigrationError::Invalid(format!(
                                "legacy Creature {legacy_id} owner is not human"
                            )));
                        }
                        owner
                    };
                    transform_legacy_creature(
                        legacy_id,
                        columns,
                        &owner_user_id,
                        email.as_deref(),
                        finance,
                        migration_time_micros,
                    )?
                }
                _ => {
                    return Err(LegacyMigrationError::Unmapped {
                        family: family.clone(),
                        key: legacy_id.clone(),
                    });
                }
            };
            capsules.extend(transformed);
        }
        capsules.extend(self.transform_reviewed_documents(migration_time_micros)?);
        capsules.extend(
            self.transform_legacy_finance(migration_time_micros, evidence.finance.as_ref())?,
        );
        capsules.extend(
            self.transform_legacy_memberships(migration_time_micros, &evidence.local_origins)?,
        );
        capsules.extend(self.transform_legacy_guest_kv(migration_time_micros)?);
        capsules.extend(self.transform_legacy_creature_types(migration_time_micros)?);
        capsules.extend(self.transform_legacy_gateway_routes(migration_time_micros)?);
        capsules.extend(self.transform_legacy_program_alarms(migration_time_micros)?);
        capsules.extend(
            self.transform_legacy_vm_resources(migration_time_micros, &evidence.path_artifacts)?,
        );
        capsules.extend(self.transform_legacy_secrets(
            migration_time_micros,
            evidence.secret_master_key.as_ref(),
        )?);
        capsules.extend(self.transform_legacy_bridges(migration_time_micros)?);
        capsules.sort_by(|left, right| {
            left.kind
                .0
                .cmp(&right.kind.0)
                .then_with(|| left.id.0.cmp(&right.id.0))
        });
        Ok(capsules)
    }

    /// Transform every reviewed `json::` document; an unreviewed key fails closed.
    fn transform_reviewed_documents(
        &self,
        migration_time_micros: i64,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let mut capsules = Vec::with_capacity(self.documents.len());
        for (key, records) in &self.documents {
            if is_legacy_finance_document_key(key)
                || key.starts_with(LEGACY_CREATURE_TYPE_PREFIX)
                || key.starts_with(LEGACY_PROXY_CORRELATION_PREFIX)
                || is_legacy_vm_resource_document_key(key)
                || key.starts_with(LEGACY_BRIDGE_GRANT_PREFIX)
            {
                continue;
            }
            let (family, legacy_id) =
                reviewed_document_family(key).ok_or_else(|| LegacyMigrationError::Unmapped {
                    family: "json-document".to_owned(),
                    key: key.clone(),
                })?;
            let (relationship, owner_scope) = match family.subject {
                LegacyDocumentSubject::Creature { relationship } => {
                    self.object("Creature", legacy_id)?;
                    (
                        CapsuleRelationship {
                            name: relationship.to_owned(),
                            target_kind: CapsuleKind("core.creature".to_owned()),
                            target_id: CapsuleId(deterministic_legacy_capsule_id(
                                "Creature",
                                legacy_id.as_bytes(),
                            )),
                        },
                        OwnerScope::Global,
                    )
                }
                LegacyDocumentSubject::Store => {
                    self.object("Store", legacy_id)?;
                    let creator = self.resolve_store_creator(legacy_id)?;
                    (
                        CapsuleRelationship {
                            name: "store".to_owned(),
                            target_kind: CapsuleKind("core.store".to_owned()),
                            target_id: CapsuleId(deterministic_legacy_capsule_id(
                                "Store",
                                legacy_id.as_bytes(),
                            )),
                        },
                        OwnerScope::Creature(required_resolved_creature(
                            "StoreMetadata",
                            &creator,
                        )?),
                    )
                }
                LegacyDocumentSubject::Program => {
                    let creature = self.resolve_program_creature(legacy_id)?;
                    (
                        CapsuleRelationship {
                            name: "program".to_owned(),
                            target_kind: CapsuleKind("core.program".to_owned()),
                            target_id: CapsuleId(deterministic_legacy_capsule_id(
                                "Program",
                                legacy_id.as_bytes(),
                            )),
                        },
                        OwnerScope::Creature(required_resolved_creature(
                            "ProgramMetadata",
                            &creature,
                        )?),
                    )
                }
            };
            capsules.push(transform_legacy_document(
                family,
                legacy_id,
                records,
                relationship,
                owner_scope,
                migration_time_micros,
            )?);
        }
        Ok(capsules)
    }

    /// Every `link::` and `index::` record must be consumed by a reviewed transform or
    /// be a derived projection that matches the reviewed objects exactly. Links also
    /// hold primary legacy state (balances, keys, VM runtime fields), so an unreviewed
    /// family fails closed instead of being dropped.
    fn verify_links_and_indexes(&self) -> LegacyMigrationResult<()> {
        let flag = |key: &str, value: &[u8]| -> LegacyMigrationResult<()> {
            if value == b"true" {
                Ok(())
            } else {
                Err(LegacyMigrationError::Invalid(format!(
                    "legacy link {key} does not hold the literal flag `true`"
                )))
            }
        };
        let diverged = |key: &str, detail: &str| {
            LegacyMigrationError::Invalid(format!("legacy link {key} diverges: {detail}"))
        };
        let mut owner_links = BTreeSet::new();
        let mut program_links = BTreeSet::new();
        for (key, value) in &self.links {
            let (family, rest) = key.split_once("::").unwrap_or((key.as_str(), ""));
            match family {
                // Consumed by the Store/Chain owner resolution and the user transform.
                "creatorof" => flag(key, value)?,
                "UserIdToEmail" => {
                    let email = String::from_utf8(value.clone())
                        .map_err(|_| diverged(key, "email is not UTF-8"))?;
                    if self.links.get(&format!("UserEmailToId::{email}"))
                        != Some(&rest.as_bytes().to_vec())
                    {
                        return Err(diverged(key, "UserEmailToId is not its exact inverse"));
                    }
                }
                "UserEmailToId" => {
                    let user = String::from_utf8(value.clone())
                        .map_err(|_| diverged(key, "user ID is not UTF-8"))?;
                    if self.links.get(&format!("UserIdToEmail::{user}"))
                        != Some(&rest.as_bytes().to_vec())
                    {
                        return Err(diverged(key, "UserIdToEmail is not its exact inverse"));
                    }
                }
                "ownerof" => {
                    flag(key, value)?;
                    let (owner, creature) = rest
                        .split_once("::")
                        .ok_or_else(|| diverged(key, "malformed owner link"))?;
                    let columns = self.object("Creature", creature)?;
                    if required_utf8_column("Creature", columns, "ownerId")? != owner {
                        return Err(diverged(key, "Creature ownerId names another owner"));
                    }
                    owner_links.insert(creature.to_owned());
                }
                "machinePrograms" => {
                    flag(key, value)?;
                    let (machine, program) = rest
                        .split_once("::")
                        .ok_or_else(|| diverged(key, "malformed program link"))?;
                    if self.resolve_program_creature(program)? != machine {
                        return Err(diverged(key, "Program machineId names another creature"));
                    }
                    program_links.insert(program.to_owned());
                }
                // Verified and exported by the ADR 0017 finance and ADR 0018 membership passes.
                family
                    if is_legacy_finance_link_family(family)
                        || is_legacy_membership_link_family(family)
                        || is_legacy_custodial_key_family(family) => {}
                // Verified against the type registry by the creature type pass.
                "CreatureTypeExists" => {}
                // ADR 0022: VMM-owned observed runtime and removed/derived control links.
                family
                    if is_legacy_observed_vm_link_family(family)
                        || is_legacy_vm_control_link_family(family)
                        || is_legacy_vm_intent_link_family(family)
                        || is_legacy_vm_resource_link_family(family)
                        || is_legacy_secret_link_family(family)
                        || is_legacy_bridge_link_family(family) => {}
                // ADR 0021 applet guest storage, resolved by the guest KV pass.
                "AppletDb" => {}
                // ADR 0021: `link::{machineId}::{guestKey}` is legacy guest `dbOp` state.
                family if self.is_legacy_guest_kv_family(family) => {}
                _ => {
                    return Err(LegacyMigrationError::Unmapped {
                        family: format!("link.{family}"),
                        key: key.clone(),
                    });
                }
            }
        }
        for ((family, legacy_id), columns) in &self.objects {
            match family.as_str() {
                "Program" if !program_links.contains(legacy_id) => {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "legacy Program {legacy_id} has no machinePrograms link"
                    )));
                }
                "Creature"
                    if required_utf8_column("Creature", columns, "type")? != "human"
                        && !owner_links.contains(legacy_id) =>
                {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "legacy Creature {legacy_id} has no ownerof link"
                    )));
                }
                _ => {}
            }
        }
        for (key, value) in &self.indexes {
            let value = String::from_utf8(value.clone()).map_err(|_| {
                LegacyMigrationError::Invalid(format!("legacy index {key} is not UTF-8"))
            })?;
            let (family, column, from) =
                if let Some(username) = key.strip_prefix("Creature::username::id::") {
                    ("Creature", "username", username)
                } else if let Some(user) = key.strip_prefix("Session::userId::id::") {
                    // Last writer wins: the index names one current session of the user.
                    ("Session", "userId", user)
                } else {
                    return Err(LegacyMigrationError::Unmapped {
                        family: "index".to_owned(),
                        key: key.clone(),
                    });
                };
            let columns = self.object(family, &value)?;
            if required_utf8_column(family, columns, column)? != from {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy index {key} names a {family} whose {column} differs"
                )));
            }
        }
        for ((family, legacy_id), columns) in &self.objects {
            if family == "Creature" {
                let username = required_utf8_column("Creature", columns, "username")?;
                let index = format!("Creature::username::id::{username}");
                if self.indexes.get(&index) != Some(&legacy_id.as_bytes().to_vec()) {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "legacy Creature {legacy_id} is not the target of its username index"
                    )));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn object(
        &self,
        family: &str,
        legacy_id: &str,
    ) -> LegacyMigrationResult<&BTreeMap<String, Vec<u8>>> {
        self.objects
            .get(&(family.to_owned(), legacy_id.to_owned()))
            .ok_or_else(|| {
                LegacyMigrationError::Invalid(format!("legacy graph omits {family} {legacy_id}"))
            })
    }

    pub(crate) fn resolve_program_creature(
        &self,
        program_id: &str,
    ) -> LegacyMigrationResult<String> {
        required_utf8_column("Program", self.object("Program", program_id)?, "machineId")
    }

    fn resolve_user_for_creature(&self, creature_id: &str) -> LegacyMigrationResult<String> {
        let columns = self.object("Creature", creature_id)?;
        if required_utf8_column("Creature", columns, "type")? == "human" {
            return Ok(creature_id.to_owned());
        }
        let owner = required_utf8_column("Creature", columns, "ownerId")?;
        let owner_columns = self.object("Creature", &owner)?;
        if required_utf8_column("Creature", owner_columns, "type")? != "human" {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy Creature {creature_id} owner is not human"
            )));
        }
        Ok(owner)
    }

    pub(crate) fn resolve_store_creator(&self, store_id: &str) -> LegacyMigrationResult<String> {
        let suffix = format!("::{store_id}");
        let mut creators = self.links.keys().filter_map(|link| {
            link.strip_prefix("creatorof::")
                .and_then(|candidate| candidate.strip_suffix(&suffix))
                .filter(|creator| !creator.is_empty())
        });
        let creator = creators.next().ok_or_else(|| {
            LegacyMigrationError::Invalid(format!("legacy Store {store_id} has no creator link"))
        })?;
        if creators.next().is_some() {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy Store {store_id} has multiple creator links"
            )));
        }
        Ok(creator.to_owned())
    }

    fn optional_link_utf8(&self, key: &str) -> LegacyMigrationResult<Option<String>> {
        self.links
            .get(key)
            .map(|value| {
                String::from_utf8(value.clone()).map_err(|_| {
                    LegacyMigrationError::Invalid(format!("legacy link {key} is not UTF-8"))
                })
            })
            .transpose()
    }
}

/// Buffers one bounded snapshot so relationships can be resolved before capsule emission.
pub struct LegacyTypedGraphTransformer {
    manifest_digest: [u8; 32],
    migration_time_micros: i64,
    records: Vec<LegacyPhysicalRecord>,
    evidence: LegacyTransformEvidence,
}

impl LegacyTypedGraphTransformer {
    #[must_use]
    pub fn new(manifest_digest: [u8; 32], migration_time_micros: i64) -> Self {
        Self {
            manifest_digest,
            migration_time_micros,
            records: Vec::new(),
            evidence: LegacyTransformEvidence::default(),
        }
    }

    #[must_use]
    pub fn with_file_artifacts(
        mut self,
        file_artifacts: BTreeMap<String, LegacyFileArtifactEvidence>,
    ) -> Self {
        self.evidence.file_artifacts = file_artifacts;
        self
    }

    #[must_use]
    pub fn with_path_artifacts(
        mut self,
        path_artifacts: BTreeMap<String, LegacyPathArtifact>,
    ) -> Self {
        self.evidence.path_artifacts = path_artifacts;
        self
    }

    #[must_use]
    pub fn with_secret_master_key(mut self, key: LegacySecretMasterKey) -> Self {
        self.evidence.secret_master_key = Some(key);
        self
    }

    #[must_use]
    pub fn with_local_origins(mut self, local_origins: BTreeSet<String>) -> Self {
        self.evidence.local_origins = local_origins;
        self
    }

    #[must_use]
    pub fn with_finance_config(mut self, finance: LegacyFinanceConfig) -> Self {
        self.evidence.finance = Some(finance);
        self
    }
}

impl LegacyTransformer for LegacyTypedGraphTransformer {
    fn manifest_digest(&self) -> [u8; 32] {
        self.manifest_digest
    }

    fn transform(
        &mut self,
        record: LegacyPhysicalRecord,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        self.records.push(record);
        Ok(Vec::new())
    }

    fn finish(&mut self) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let records = std::mem::take(&mut self.records);
        LegacySnapshotGraph::assemble(records)?
            .transform_reviewed_with_evidence(self.migration_time_micros, &self.evidence)
    }
}
