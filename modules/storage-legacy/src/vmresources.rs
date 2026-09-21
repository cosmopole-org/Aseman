//! ADR 0022: VM resource stores/entities, proxy entity configuration, and deployed
//! entity artifacts are durable intent and migrate with file-copy evidence.

use super::*;

pub const LEGACY_RESOURCE_STORE_PREFIX: &str = "Json::VmResourceStore::";
pub const LEGACY_RESOURCE_ENTITY_PREFIX: &str = "Json::VmResourceEntity::";
pub const LEGACY_PROXY_ENTITY_PREFIX: &str = "Json::ProxyEntity::";
const ARTIFACT_LINKS: [&str; 3] = ["vmEntityPath", "vmEntityType", "vmEntityDownloadable"];

/// `true` for a `json::` key family migrated by this module.
#[must_use]
pub fn is_legacy_vm_resource_document_key(key: &str) -> bool {
    key.starts_with(LEGACY_RESOURCE_STORE_PREFIX)
        || key.starts_with(LEGACY_RESOURCE_ENTITY_PREFIX)
        || key.starts_with(LEGACY_PROXY_ENTITY_PREFIX)
}

/// `true` for a link family migrated or verified by this module.
#[must_use]
pub fn is_legacy_vm_resource_link_family(family: &str) -> bool {
    family == "vmOwnedStore" || ARTIFACT_LINKS.contains(&family)
}

/// Verify every root of one legacy JSON key against the allowed root names.
fn document_roots(
    key: &str,
    records: &BTreeMap<String, Vec<u8>>,
    allowed: &[&str],
) -> LegacyMigrationResult<BTreeMap<String, Map<String, Value>>> {
    let mut grouped: BTreeMap<&str, BTreeMap<String, Vec<u8>>> = BTreeMap::new();
    for (path, value) in records {
        let root = path.split('.').next().unwrap_or("");
        let root = allowed.iter().find(|name| **name == root).ok_or_else(|| {
            LegacyMigrationError::Unmapped {
                family: "json-document-path".to_owned(),
                key: format!("json::{key}::{path}"),
            }
        })?;
        grouped
            .entry(root)
            .or_default()
            .insert(path.clone(), value.clone());
    }
    grouped
        .into_iter()
        .map(|(root, records)| {
            Ok((
                root.to_owned(),
                verified_legacy_document(key, root, &records)?,
            ))
        })
        .collect()
}

fn document_fields(
    key: &str,
    path: &str,
    document: Map<String, Value>,
) -> LegacyMigrationResult<BTreeMap<String, CapsuleValue>> {
    let entry_count = i64::try_from(document.len()).map_err(|_| {
        LegacyMigrationError::Invalid(format!("legacy document {key} is too large"))
    })?;
    let document = legacy_json_to_capsule_value(key, &Value::Object(document))?;
    let content_digest = legacy_document_digest(&document)?;
    Ok(BTreeMap::from([
        ("document".to_owned(), document),
        (
            "document_path".to_owned(),
            CapsuleValue::Text(path.to_owned()),
        ),
        ("entry_count".to_owned(), CapsuleValue::Integer(entry_count)),
        (
            "content_digest".to_owned(),
            CapsuleValue::Bytes(content_digest),
        ),
    ]))
}

fn artifact_fields(
    path: &str,
    artifacts: &BTreeMap<String, LegacyPathArtifact>,
) -> LegacyMigrationResult<BTreeMap<String, CapsuleValue>> {
    match artifacts.get(path) {
        Some(LegacyPathArtifact::Present(evidence)) => Ok(BTreeMap::from([
            ("artifact_present".to_owned(), CapsuleValue::Bool(true)),
            (
                "store_key".to_owned(),
                CapsuleValue::Text(evidence.store_key.clone()),
            ),
            (
                "artifact_digest".to_owned(),
                CapsuleValue::Bytes(evidence.content_digest.to_vec()),
            ),
            (
                "size_bytes".to_owned(),
                CapsuleValue::Integer(i64::try_from(evidence.size_bytes).map_err(|_| {
                    LegacyMigrationError::Invalid(format!("legacy artifact {path} is too large"))
                })?),
            ),
            (
                "media_type".to_owned(),
                CapsuleValue::Text(evidence.media_type.clone()),
            ),
        ])),
        Some(LegacyPathArtifact::AttestedAbsent) => Ok(BTreeMap::from([(
            "artifact_present".to_owned(),
            CapsuleValue::Bool(false),
        )])),
        None => Err(LegacyMigrationError::Unmapped {
            family: "artifact.evidence".to_owned(),
            key: path.to_owned(),
        }),
    }
}

fn relationship(name: &str, kind: &str, family: &str, legacy_id: &str) -> CapsuleRelationship {
    CapsuleRelationship {
        name: name.to_owned(),
        target_kind: CapsuleKind(kind.to_owned()),
        target_id: CapsuleId(deterministic_legacy_capsule_id(
            family,
            legacy_id.as_bytes(),
        )),
    }
}

impl LegacySnapshotGraph {
    /// Resolve a legacy `machineId` that names either a creature or a program.
    fn resolve_machine_owner(&self, machine: &str) -> LegacyMigrationResult<String> {
        if self
            .objects
            .contains_key(&("Creature".to_owned(), machine.to_owned()))
        {
            return Ok(machine.to_owned());
        }
        if self
            .objects
            .contains_key(&("Program".to_owned(), machine.to_owned()))
        {
            return self.resolve_program_creature(machine);
        }
        Err(LegacyMigrationError::Invalid(format!(
            "legacy machine {machine} names no local creature or program"
        )))
    }

    pub(crate) fn transform_legacy_vm_resources(
        &self,
        migration_time_micros: i64,
        artifacts: &BTreeMap<String, LegacyPathArtifact>,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let mut capsules = Vec::new();
        let mut store_owners = BTreeMap::new();
        let mut owned_links = BTreeSet::new();
        for (key, value) in &self.links {
            if let Some(rest) = key.strip_prefix("vmOwnedStore::") {
                let pair = rest
                    .split_once("::")
                    .filter(|(machine, store)| !machine.is_empty() && !store.is_empty());
                if value != b"true" || pair.is_none() {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "legacy {key} is not a reviewed ownership flag"
                    )));
                }
                owned_links.insert(rest.to_owned());
            }
        }
        for (key, records) in &self.documents {
            let Some(store) = key.strip_prefix(LEGACY_RESOURCE_STORE_PREFIX) else {
                continue;
            };
            let mut roots = document_roots(key, records, &["core", "metadata"])?;
            let core = roots.remove("core").ok_or_else(|| {
                LegacyMigrationError::Invalid(format!(
                    "legacy resource store {store} has no core record"
                ))
            })?;
            let text = |name: &str| {
                core.get(name)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned()
            };
            let (id, name, machine) = (text("id"), text("name"), text("machineId"));
            if id != store || machine.is_empty() || core.len() != 3 {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy resource store {store} core record is inconsistent or unowned"
                )));
            }
            if !owned_links.remove(&format!("{machine}::{store}")) {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy resource store {store} has no vmOwnedStore link"
                )));
            }
            let creature = self.resolve_machine_owner(&machine)?;
            let mut body = document_fields(
                key,
                "metadata",
                roots.remove("metadata").unwrap_or_default(),
            )?;
            body.insert("name".to_owned(), CapsuleValue::Text(name));
            body.insert("machine_ref".to_owned(), CapsuleValue::Text(machine));
            store_owners.insert(store.to_owned(), creature.clone());
            capsules.push(seal_legacy_capsule(
                LegacyCapsuleSpec {
                    family: "VmResourceStore",
                    kind: "core.vm_resource_store",
                    storage_class: StorageClass::Core,
                    owner_scope: OwnerScope::Creature(required_resolved_creature(
                        "VmResourceStore",
                        &creature,
                    )?),
                    migration_time_micros,
                },
                store,
                vec![relationship(
                    "creature",
                    "core.creature",
                    "Creature",
                    &creature,
                )],
                body,
            )?);
        }
        if let Some(orphan) = owned_links.iter().next() {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy vmOwnedStore::{orphan} names no resource store"
            )));
        }
        for (key, records) in &self.documents {
            let Some(rest) = key.strip_prefix(LEGACY_RESOURCE_ENTITY_PREFIX) else {
                continue;
            };
            let parts = rest.split("::").collect::<Vec<_>>();
            let [store, entity_type, entity] = parts.as_slice() else {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy resource entity key {key} is not store::type::id"
                )));
            };
            let creature = store_owners.get(*store).ok_or_else(|| {
                LegacyMigrationError::Invalid(format!(
                    "legacy resource entity {rest} names no resource store"
                ))
            })?;
            let mut roots = document_roots(key, records, &["payload", "meta"])?;
            let meta = roots.remove("meta").ok_or_else(|| {
                LegacyMigrationError::Invalid(format!(
                    "legacy resource entity {rest} has no meta record"
                ))
            })?;
            let text = |name: &str| meta.get(name).and_then(Value::as_str).unwrap_or("");
            if text("id") != *entity
                || text("storeId") != *store
                || text("entityType") != *entity_type
                || text("path").is_empty()
                || meta.len() != 4
            {
                return Err(LegacyMigrationError::Invalid(format!(
                    "legacy resource entity {rest} meta disagrees with its key"
                )));
            }
            let mut body =
                document_fields(key, "payload", roots.remove("payload").unwrap_or_default())?;
            body.extend(artifact_fields(text("path"), artifacts)?);
            body.insert(
                "entity_type".to_owned(),
                CapsuleValue::Text((*entity_type).to_owned()),
            );
            body.insert(
                "entity_ref".to_owned(),
                CapsuleValue::Text((*entity).to_owned()),
            );
            capsules.push(seal_legacy_capsule(
                LegacyCapsuleSpec {
                    family: "VmResourceEntity",
                    kind: "core.vm_resource_entity",
                    storage_class: StorageClass::Core,
                    owner_scope: OwnerScope::Creature(required_resolved_creature(
                        "VmResourceEntity",
                        creature,
                    )?),
                    migration_time_micros,
                },
                rest,
                vec![relationship(
                    "resource_store",
                    "core.vm_resource_store",
                    "VmResourceStore",
                    store,
                )],
                body,
            )?);
        }
        for (key, records) in &self.documents {
            let Some(program_entity) = key.strip_prefix(LEGACY_PROXY_ENTITY_PREFIX) else {
                continue;
            };
            let columns = self.object("Entity", program_entity)?;
            let program = required_utf8_column("Entity", columns, "programId")?;
            let creature = self.resolve_program_creature(&program)?;
            let mut roots = document_roots(key, records, &["config"])?;
            let body = document_fields(key, "config", roots.remove("config").unwrap_or_default())?;
            capsules.push(seal_legacy_capsule(
                LegacyCapsuleSpec {
                    family: "EntityConfig",
                    kind: "core.entity_config",
                    storage_class: StorageClass::Core,
                    owner_scope: OwnerScope::Creature(required_resolved_creature(
                        "EntityConfig",
                        &creature,
                    )?),
                    migration_time_micros,
                },
                program_entity,
                vec![relationship(
                    "entity",
                    "core.entity",
                    "Entity",
                    program_entity,
                )],
                body,
            )?);
        }
        capsules.extend(self.transform_legacy_entity_artifacts(migration_time_micros, artifacts)?);
        Ok(capsules)
    }

    /// Deployed entity artifacts: `vmEntityPath` (primary) and `vmEntityDownloadable`
    /// need copy evidence; `vmEntityType` must equal the entity's recorded type.
    fn transform_legacy_entity_artifacts(
        &self,
        migration_time_micros: i64,
        artifacts: &BTreeMap<String, LegacyPathArtifact>,
    ) -> LegacyMigrationResult<Vec<CapsuleEnvelope>> {
        let mut capsules = Vec::new();
        for (key, value) in &self.links {
            let Some((family, program_entity)) = key.split_once("::") else {
                continue;
            };
            if !ARTIFACT_LINKS.contains(&family) {
                continue;
            }
            let columns = self.object("Entity", program_entity)?;
            let text = String::from_utf8(value.clone())
                .ok()
                .filter(|text| !text.is_empty())
                .ok_or_else(|| {
                    LegacyMigrationError::Invalid(format!("legacy {key} is empty or not UTF-8"))
                })?;
            let entity_type = required_utf8_column("Entity", columns, "entityType")?;
            let has = |name: &str| {
                self.links
                    .contains_key(&format!("{name}::{program_entity}"))
            };
            let role = match family {
                "vmEntityType" => {
                    if text != entity_type || !has("vmEntityPath") {
                        return Err(LegacyMigrationError::Invalid(format!(
                            "legacy {key} disagrees with the entity or has no artifact path"
                        )));
                    }
                    continue;
                }
                "vmEntityPath" if !has("vmEntityType") => {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "legacy {key} has no vmEntityType"
                    )));
                }
                "vmEntityPath" => "primary",
                _ => "downloadable",
            };
            let program = required_utf8_column("Entity", columns, "programId")?;
            let creature = self.resolve_program_creature(&program)?;
            let mut body = artifact_fields(&text, artifacts)?;
            body.insert(
                "artifact_role".to_owned(),
                CapsuleValue::Text(role.to_owned()),
            );
            capsules.push(seal_legacy_capsule(
                LegacyCapsuleSpec {
                    family: "EntityArtifact",
                    kind: "core.entity_artifact",
                    storage_class: StorageClass::Core,
                    owner_scope: OwnerScope::Creature(required_resolved_creature(
                        "EntityArtifact",
                        &creature,
                    )?),
                    migration_time_micros,
                },
                &format!("{program_entity}\0{role}"),
                vec![relationship(
                    "entity",
                    "core.entity",
                    "Entity",
                    program_entity,
                )],
                body,
            )?);
        }
        Ok(capsules)
    }
}
