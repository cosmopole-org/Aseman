//! Program entity repositories on the capsule protocol (RL-004 strangler, target side).
//!
//! Stored exactly as the A308 export writes them:
//! - a `core.entity` capsule per `{program}::{entity}`, scoped like its program and
//!   related to it, with a `core.legacy_identity` row;
//! - a `core.entity_artifact` capsule per entity and role, carrying the blob evidence
//!   of the file (ADR 0027), never its bytes;
//! - the `core.entity_config` document (legacy `Json::ProxyEntity`);
//! - a `core.vm_resource_entity` capsule per resource entity: its payload document and
//!   the evidence of its data file, scoped like its resource store.
//!
//! Legacy's `vmEntityType` link is derived: it is the entity's type whenever the
//! entity has a primary file.

use crate::store::{body, next_revision, port_error};
use crate::support::{
    Capsules, DocumentFamily, MAX_CAS_ATTEMPTS, failed, legacy_identity, new_capsule, relationship,
    text, tombstone,
};
use crate::{CapsuleStore, CapsuleStoreError};
use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleValue, StorageClass};
use aseman_contracts::legacy_documents::{
    capsule_value_to_json, legacy_document_fields, merge_legacy_objects,
};
use aseman_contracts::legacy_realtime::deterministic_legacy_capsule_id;
use aseman_domain::blob::BlobEvidence;
use aseman_domain::program::{
    ArtifactRole, EntityArtifact, EntityRecord, ResourceEntityRef, VmResourceEntity,
};
use aseman_ports::{EntityDirectory, PortError, PortResult, VmResourceEntities};
use std::collections::BTreeMap;

const ENTITY: &str = "core.entity";
const ENTITY_ARTIFACT: &str = "core.entity_artifact";
const PROGRAM: &str = "core.program";

/// Entity ports over any [`CapsuleStore`].
pub struct CapsuleEntityPorts<'a> {
    pub repository: &'a dyn CapsuleStore,
}

/// The legacy identity of an entity.
fn entity_key(program_id: &str, entity_id: &str) -> String {
    [program_id, "::", entity_id].concat()
}

fn entity_capsule_id(key: &str) -> [u8; 16] {
    deterministic_legacy_capsule_id("Entity", key.as_bytes())
}

fn artifact_id(key: &str, role: ArtifactRole) -> [u8; 16] {
    deterministic_legacy_capsule_id(
        "EntityArtifact",
        [key, "\0", role.as_str()].concat().as_bytes(),
    )
}

/// The blob evidence of a stored file, as the A308 export records it.
fn artifact_fields(evidence: &BlobEvidence) -> PortResult<BTreeMap<String, CapsuleValue>> {
    Ok(BTreeMap::from([
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
            CapsuleValue::Integer(
                i64::try_from(evidence.size_bytes).map_err(|_| failed("file is too large"))?,
            ),
        ),
        (
            "media_type".to_owned(),
            CapsuleValue::Text(evidence.media_type.clone()),
        ),
    ]))
}

const ENTITY_CONFIG: DocumentFamily = DocumentFamily {
    kind: "core.entity_config",
    family: "EntityConfig",
    key_prefix: "Json::ProxyEntity::",
    subject_relationship: "entity",
    subject_kind: ENTITY,
    subject_family: "Entity",
    root: "config",
};

impl CapsuleEntityPorts<'_> {
    fn capsules(&self) -> Capsules<'_> {
        Capsules(self.repository)
    }

    /// Write `fields` as the next revision of capsule `id`, or as its first revision
    /// built by `create` (which also returns any companion capsules).
    fn upsert(
        &self,
        kind_name: &str,
        id: [u8; 16],
        fields: &BTreeMap<String, CapsuleValue>,
        create: impl Fn() -> PortResult<Vec<CapsuleEnvelope>>,
    ) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let written = match self.capsules().get(kind_name, id)? {
                Some(current) => self.repository.put(
                    &next_revision(&current, fields.clone())?,
                    Some(current.revision),
                ),
                None => self.repository.put_all(
                    &create()?
                        .into_iter()
                        .map(|capsule| (capsule, None))
                        .collect::<Vec<_>>(),
                ),
            };
            match written {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }
}

impl EntityDirectory for CapsuleEntityPorts<'_> {
    fn entity(&self, program_id: &str, entity_id: &str) -> PortResult<Option<EntityRecord>> {
        let Some(capsule) = self.capsules().live(
            ENTITY,
            entity_capsule_id(&entity_key(program_id, entity_id)),
        )?
        else {
            return Ok(None);
        };
        let fields = body(&capsule).ok_or(PortError::NotFound)?;
        Ok(Some(EntityRecord {
            program_id: program_id.to_owned(),
            entity_id: text(fields, "entity_name"),
            entity_type: text(fields, "entity_type"),
            image_name: text(fields, "image_name"),
        }))
    }

    fn put_entity(&self, entity: &EntityRecord) -> PortResult<()> {
        let program_capsule =
            deterministic_legacy_capsule_id("Program", entity.program_id.as_bytes());
        let program = self
            .capsules()
            .live(PROGRAM, program_capsule)?
            .ok_or(PortError::NotFound)?;
        let key = entity_key(&entity.program_id, &entity.entity_id);
        let fields = BTreeMap::from([
            (
                "entity_name".to_owned(),
                CapsuleValue::Text(entity.entity_id.clone()),
            ),
            (
                "entity_type".to_owned(),
                CapsuleValue::Text(entity.entity_type.clone()),
            ),
            (
                "image_name".to_owned(),
                CapsuleValue::Text(entity.image_name.clone()),
            ),
        ]);
        self.upsert(ENTITY, entity_capsule_id(&key), &fields, || {
            Ok(vec![
                new_capsule(
                    entity_capsule_id(&key),
                    ENTITY,
                    StorageClass::Core,
                    program.owner_scope.clone(),
                    vec![relationship("program", PROGRAM, program_capsule)],
                    fields.clone(),
                )?,
                legacy_identity("Entity", &key, ENTITY)?,
            ])
        })
    }

    fn artifact(
        &self,
        program_id: &str,
        entity_id: &str,
        role: ArtifactRole,
    ) -> PortResult<Option<EntityArtifact>> {
        let key = entity_key(program_id, entity_id);
        let Some(capsule) = self
            .capsules()
            .live(ENTITY_ARTIFACT, artifact_id(&key, role))?
        else {
            return Ok(None);
        };
        let fields = body(&capsule).ok_or(PortError::NotFound)?;
        Ok(Some(EntityArtifact {
            store_key: match fields.get("artifact_present") {
                Some(CapsuleValue::Bool(true)) => Some(text(fields, "store_key")),
                Some(CapsuleValue::Bool(false)) => None,
                _ => return Err(failed("entity artifact has no presence")),
            },
        }))
    }

    fn put_artifact(
        &self,
        program_id: &str,
        entity_id: &str,
        role: ArtifactRole,
        evidence: &BlobEvidence,
    ) -> PortResult<()> {
        let key = entity_key(program_id, entity_id);
        let entity = self
            .capsules()
            .live(ENTITY, entity_capsule_id(&key))?
            .ok_or(PortError::NotFound)?;
        let mut fields = artifact_fields(evidence)?;
        fields.insert(
            "artifact_role".to_owned(),
            CapsuleValue::Text(role.as_str().to_owned()),
        );
        self.upsert(ENTITY_ARTIFACT, artifact_id(&key, role), &fields, || {
            Ok(vec![new_capsule(
                artifact_id(&key, role),
                ENTITY_ARTIFACT,
                StorageClass::Core,
                entity.owner_scope.clone(),
                vec![relationship("entity", ENTITY, entity_capsule_id(&key))],
                fields.clone(),
            )?])
        })
    }

    fn deployed_programs(&self) -> PortResult<Vec<String>> {
        let programs = self.capsules().legacy_ids("Program")?;
        let mut deployed = Vec::new();
        for (id, key) in self.capsules().legacy_ids("Entity")? {
            if self
                .capsules()
                .live(ENTITY_ARTIFACT, artifact_id(&key, ArtifactRole::Primary))?
                .is_none()
            {
                continue;
            }
            let Some(entity) = self.capsules().live(ENTITY, id)? else {
                continue;
            };
            let program = entity
                .relationships
                .iter()
                .find(|relationship| relationship.name == "program")
                .and_then(|relationship| programs.get(&relationship.target_id.0))
                .ok_or_else(|| failed(format!("entity {key} names no program")))?;
            deployed.push(program.clone());
        }
        deployed.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        deployed.dedup();
        Ok(deployed)
    }

    fn entity_config(&self, program_id: &str, entity_id: &str) -> PortResult<Option<String>> {
        self.capsules().document_at(
            &ENTITY_CONFIG,
            &entity_key(program_id, entity_id),
            ENTITY_CONFIG.root,
        )
    }

    fn merge_entity_config(
        &self,
        program_id: &str,
        entity_id: &str,
        document: &str,
    ) -> PortResult<()> {
        self.capsules()
            .merge_document(&ENTITY_CONFIG, &entity_key(program_id, entity_id), document)
    }
}

const RESOURCE_ENTITY: &str = "core.vm_resource_entity";
const RESOURCE_STORE: &str = "core.vm_resource_store";

fn resource_entity_id(entity: &ResourceEntityRef) -> [u8; 16] {
    deterministic_legacy_capsule_id("VmResourceEntity", entity.legacy_id().as_bytes())
}

fn valid(entity: &ResourceEntityRef) -> PortResult<()> {
    if entity.is_valid() {
        Ok(())
    } else {
        Err(failed(format!(
            "invalid resource entity {}",
            entity.legacy_id()
        )))
    }
}

impl VmResourceEntities for CapsuleEntityPorts<'_> {
    fn resource_entity(&self, entity: &ResourceEntityRef) -> PortResult<Option<VmResourceEntity>> {
        valid(entity)?;
        let Some(capsule) = self
            .capsules()
            .live(RESOURCE_ENTITY, resource_entity_id(entity))?
        else {
            return Ok(None);
        };
        let fields = body(&capsule).ok_or(PortError::NotFound)?;
        let payload = match fields.get("document").map(capsule_value_to_json) {
            Some(Ok(document @ serde_json::Value::Object(_))) => {
                serde_json::to_string(&document).map_err(failed)?
            }
            _ => return Err(failed("resource entity has no payload document")),
        };
        Ok(Some(VmResourceEntity {
            reference: entity.clone(),
            payload,
            data_key: match fields.get("artifact_present") {
                Some(CapsuleValue::Bool(true)) => Some(text(fields, "store_key")),
                Some(CapsuleValue::Bool(false)) => None,
                _ => return Err(failed("resource entity has no data presence")),
            },
        }))
    }

    fn put_resource_entity(
        &self,
        entity: &ResourceEntityRef,
        payload: &str,
        data: &BlobEvidence,
    ) -> PortResult<()> {
        valid(entity)?;
        let Ok(serde_json::Value::Object(incoming)) = serde_json::from_str(payload) else {
            return Err(failed("payload must be a JSON object"));
        };
        let store_capsule =
            deterministic_legacy_capsule_id("VmResourceStore", entity.store_id.as_bytes());
        let store = self
            .capsules()
            .live(RESOURCE_STORE, store_capsule)?
            .ok_or(PortError::NotFound)?;
        let id = resource_entity_id(entity);
        let key = ["Json::VmResourceEntity::", &entity.legacy_id()].concat();
        for _ in 0..MAX_CAS_ATTEMPTS {
            let current = self.capsules().get(RESOURCE_ENTITY, id)?;
            let mut document = match current
                .as_ref()
                .and_then(body)
                .and_then(|fields| fields.get("document"))
                .map(capsule_value_to_json)
            {
                Some(Ok(serde_json::Value::Object(document))) => document,
                _ => serde_json::Map::new(),
            };
            merge_legacy_objects(&mut document, &incoming);
            let mut fields = legacy_document_fields(&key, "payload", &document).map_err(failed)?;
            fields.extend([
                (
                    "entity_type".to_owned(),
                    CapsuleValue::Text(entity.entity_type.clone()),
                ),
                (
                    "entity_ref".to_owned(),
                    CapsuleValue::Text(entity.entity_id.clone()),
                ),
            ]);
            fields.extend(artifact_fields(data)?);
            let written = match current {
                Some(current) => self
                    .repository
                    .put(&next_revision(&current, fields)?, Some(current.revision)),
                None => self.repository.put(
                    &new_capsule(
                        id,
                        RESOURCE_ENTITY,
                        StorageClass::Core,
                        store.owner_scope.clone(),
                        vec![relationship(
                            "resource_store",
                            RESOURCE_STORE,
                            store_capsule,
                        )],
                        fields,
                    )?,
                    None,
                ),
            };
            match written {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }

    fn delete_resource_entity(&self, entity: &ResourceEntityRef) -> PortResult<()> {
        valid(entity)?;
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some(current) = self
                .capsules()
                .live(RESOURCE_ENTITY, resource_entity_id(entity))?
            else {
                return Ok(());
            };
            match self
                .repository
                .put(&tombstone(&current)?, Some(current.revision))
            {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }
}
