//! Program entities behind [`EntityDirectory`] (RL-004 strangler), routed per ADR 0026.
//!
//! The legacy adapter keeps the legacy encodings:
//! - the `Entity` object keyed `{program}::{entity}`;
//! - `vmEntityPath` / `vmEntityDownloadable` links holding the file's local path;
//! - the derived `vmEntityType` link, written with the primary file and kept equal to
//!   the entity's type (the A308 export rejects a disagreement);
//! - the `Json::ProxyEntity` configuration document;
//! - `Json::VmResourceEntity` payload and meta documents, the meta holding the data
//!   file's local path.
//!
//! Legacy records files by absolute path, so the adapter maps them to and from blob
//! keys (ADR 0027) through the node's storage-root blob store.

use aseman_domain::blob::BlobEvidence;
use aseman_domain::program::{
    ArtifactRole, EntityArtifact, EntityRecord, ResourceEntityRef, VmResourceEntity,
};
use aseman_ports::{BlobStore, EntityDirectory, PortError, PortResult, VmResourceEntities};

use crate::drivers::blob_store::StorageRootBlobStore;
use crate::models::transaction::ITrx;
use crate::shell::api::model::program_ports::resource_store_key;
use crate::shell::api::model::{Entity, Program};

fn failed(error: impl ToString) -> PortError {
    PortError::Failed(error.to_string())
}

fn entity_key(program_id: &str, entity_id: &str) -> String {
    [program_id, "::", entity_id].concat()
}

fn artifact_link(role: ArtifactRole, key: &str) -> String {
    match role {
        ArtifactRole::Primary => ["vmEntityPath::", key].concat(),
        ArtifactRole::Downloadable => ["vmEntityDownloadable::", key].concat(),
    }
}

fn type_link(key: &str) -> String {
    ["vmEntityType::", key].concat()
}

fn config_key(key: &str) -> String {
    ["Json::ProxyEntity::", key].concat()
}

const TYPE_LINKS: &str = "link::vmEntityType::";

/// The legacy adapter.
struct LegacyEntities<'a> {
    trx: &'a dyn ITrx,
    blobs: &'a StorageRootBlobStore,
}

impl LegacyEntities<'_> {
    fn exists(&self, key: &str) -> bool {
        self.trx.has_obj(Entity::type_(), key)
    }
}

impl EntityDirectory for LegacyEntities<'_> {
    fn entity(&self, program_id: &str, entity_id: &str) -> PortResult<Option<EntityRecord>> {
        if !self.exists(&entity_key(program_id, entity_id)) {
            return Ok(None);
        }
        let entity = Entity {
            program_id: program_id.to_owned(),
            entity_id: entity_id.to_owned(),
            ..Default::default()
        }
        .pull(self.trx);
        Ok(Some(EntityRecord {
            program_id: entity.program_id,
            entity_id: entity.entity_id,
            entity_type: entity.entity_type,
            image_name: entity.image_name,
        }))
    }

    fn put_entity(&self, entity: &EntityRecord) -> PortResult<()> {
        if !self.trx.has_obj(Program::type_(), &entity.program_id) {
            return Err(PortError::NotFound);
        }
        Entity {
            program_id: entity.program_id.clone(),
            entity_id: entity.entity_id.clone(),
            entity_type: entity.entity_type.clone(),
            image_name: entity.image_name.clone(),
        }
        .push(self.trx);
        let key = entity_key(&entity.program_id, &entity.entity_id);
        if !self
            .trx
            .get_link(&artifact_link(ArtifactRole::Primary, &key))
            .is_empty()
        {
            self.trx.put_link(&type_link(&key), &entity.entity_type);
        }
        Ok(())
    }

    fn artifact(
        &self,
        program_id: &str,
        entity_id: &str,
        role: ArtifactRole,
    ) -> PortResult<Option<EntityArtifact>> {
        let path = self
            .trx
            .get_link(&artifact_link(role, &entity_key(program_id, entity_id)));
        if path.is_empty() {
            return Ok(None);
        }
        let store_key = self
            .blobs
            .key_of(&path)
            .ok_or_else(|| failed(format!("entity file {path} is outside the storage root")))?;
        Ok(Some(EntityArtifact {
            store_key: Some(store_key),
        }))
    }

    fn put_artifact(
        &self,
        program_id: &str,
        entity_id: &str,
        role: ArtifactRole,
        evidence: &BlobEvidence,
    ) -> PortResult<()> {
        let Some(entity) = self.entity(program_id, entity_id)? else {
            return Err(PortError::NotFound);
        };
        let path = self.blobs.local_path(&evidence.store_key)?;
        let path = path
            .to_str()
            .ok_or_else(|| failed("entity file path is not UTF-8"))?;
        let key = entity_key(program_id, entity_id);
        self.trx.put_link(&artifact_link(role, &key), path);
        if role == ArtifactRole::Primary {
            self.trx.put_link(&type_link(&key), &entity.entity_type);
        }
        Ok(())
    }

    fn deployed_programs(&self) -> PortResult<Vec<String>> {
        let mut programs = self
            .trx
            .get_by_prefix(TYPE_LINKS)
            .into_iter()
            .filter_map(|link| {
                let key = link.strip_prefix(TYPE_LINKS)?;
                let (program, _) = key.split_once("::")?;
                (!program.is_empty()).then(|| program.to_owned())
            })
            .collect::<Vec<_>>();
        programs.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        programs.dedup();
        Ok(programs)
    }

    fn entity_config(&self, program_id: &str, entity_id: &str) -> PortResult<Option<String>> {
        match self
            .trx
            .get_json(&config_key(&entity_key(program_id, entity_id)), "config")
        {
            Ok(config) => serde_json::to_string(&config).map(Some).map_err(failed),
            Err(_) => Ok(None),
        }
    }

    fn merge_entity_config(
        &self,
        program_id: &str,
        entity_id: &str,
        document: &str,
    ) -> PortResult<()> {
        let key = entity_key(program_id, entity_id);
        if !self.exists(&key) {
            return Err(PortError::NotFound);
        }
        let document = match serde_json::from_str::<serde_json::Value>(document) {
            Ok(object @ serde_json::Value::Object(_)) => object,
            _ => return Err(failed("config must be a JSON object")),
        };
        self.trx
            .put_json(&config_key(&key), "config", &document, true)
            .map_err(failed)
    }
}

fn resource_entity_key(entity: &ResourceEntityRef) -> String {
    ["Json::VmResourceEntity::", &entity.legacy_id()].concat()
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

/// Legacy keeps the data file's local path in `meta.path`.
impl VmResourceEntities for LegacyEntities<'_> {
    fn resource_entity(&self, entity: &ResourceEntityRef) -> PortResult<Option<VmResourceEntity>> {
        valid(entity)?;
        let key = resource_entity_key(entity);
        let Ok(meta) = self.trx.get_json(&key, "meta") else {
            return Ok(None);
        };
        let path = meta
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let data_key = self.blobs.key_of(path).ok_or_else(|| {
            failed(format!(
                "resource entity file {path} is outside the storage root"
            ))
        })?;
        let payload = self.trx.get_json(&key, "payload").unwrap_or_default();
        Ok(Some(VmResourceEntity {
            reference: entity.clone(),
            payload: serde_json::to_string(&payload).map_err(failed)?,
            data_key: Some(data_key),
        }))
    }

    fn put_resource_entity(
        &self,
        entity: &ResourceEntityRef,
        payload: &str,
        data: &BlobEvidence,
    ) -> PortResult<()> {
        valid(entity)?;
        let payload = match serde_json::from_str::<serde_json::Value>(payload) {
            Ok(object @ serde_json::Value::Object(_)) => object,
            _ => return Err(failed("payload must be a JSON object")),
        };
        if self
            .trx
            .get_json(&resource_store_key(&entity.store_id), "core")
            .is_err()
        {
            return Err(PortError::NotFound);
        }
        let path = self.blobs.local_path(&data.store_key)?;
        let key = resource_entity_key(entity);
        self.trx
            .put_json(&key, "payload", &payload, true)
            .map_err(failed)?;
        let meta = serde_json::json!({
            "id": entity.entity_id,
            "storeId": entity.store_id,
            "entityType": entity.entity_type,
            "path": path.to_string_lossy(),
        });
        self.trx.put_json(&key, "meta", &meta, true).map_err(failed)
    }

    fn delete_resource_entity(&self, entity: &ResourceEntityRef) -> PortResult<()> {
        valid(entity)?;
        // LD-26: the documents are really removed (legacy deleted raw keys that the
        // JSON encoding never writes).
        let key = resource_entity_key(entity);
        self.trx.del_json(&key, "payload");
        self.trx.del_json(&key, "meta");
        Ok(())
    }
}

/// The entity ports of one state action, routed per ADR 0026 to the action's
/// PostgreSQL unit of work when the node runs on PostgreSQL, else to legacy.
pub(crate) struct EntityPorts<'a> {
    pub(crate) trx: &'a dyn ITrx,
    /// Where the node keeps file bytes; legacy records files by local path.
    pub(crate) blobs: &'a StorageRootBlobStore,
}

/// Run `$call` on the adapter for the current provider, bound as `$ports`.
macro_rules! route {
    ($self:ident, |$ports:ident| $call:expr) => {
        match crate::shell::api::model::core_storage::current_unit() {
            Some(unit) => {
                let $ports = aseman_capsule::entity::CapsuleEntityPorts { repository: &*unit };
                $call
            }
            None => {
                let $ports = LegacyEntities {
                    trx: $self.trx,
                    blobs: $self.blobs,
                };
                $call
            }
        }
    };
}

impl EntityDirectory for EntityPorts<'_> {
    fn entity(&self, program_id: &str, entity_id: &str) -> PortResult<Option<EntityRecord>> {
        route!(self, |ports| ports.entity(program_id, entity_id))
    }

    fn put_entity(&self, entity: &EntityRecord) -> PortResult<()> {
        route!(self, |ports| ports.put_entity(entity))
    }

    fn artifact(
        &self,
        program_id: &str,
        entity_id: &str,
        role: ArtifactRole,
    ) -> PortResult<Option<EntityArtifact>> {
        route!(self, |ports| ports.artifact(program_id, entity_id, role))
    }

    fn put_artifact(
        &self,
        program_id: &str,
        entity_id: &str,
        role: ArtifactRole,
        evidence: &BlobEvidence,
    ) -> PortResult<()> {
        route!(self, |ports| ports
            .put_artifact(program_id, entity_id, role, evidence))
    }

    fn deployed_programs(&self) -> PortResult<Vec<String>> {
        route!(self, |ports| ports.deployed_programs())
    }

    fn entity_config(&self, program_id: &str, entity_id: &str) -> PortResult<Option<String>> {
        route!(self, |ports| ports.entity_config(program_id, entity_id))
    }

    fn merge_entity_config(
        &self,
        program_id: &str,
        entity_id: &str,
        document: &str,
    ) -> PortResult<()> {
        route!(self, |ports| ports
            .merge_entity_config(program_id, entity_id, document))
    }
}

impl VmResourceEntities for EntityPorts<'_> {
    fn resource_entity(&self, entity: &ResourceEntityRef) -> PortResult<Option<VmResourceEntity>> {
        route!(self, |ports| ports.resource_entity(entity))
    }

    fn put_resource_entity(
        &self,
        entity: &ResourceEntityRef,
        payload: &str,
        data: &BlobEvidence,
    ) -> PortResult<()> {
        route!(self, |ports| ports
            .put_resource_entity(entity, payload, data))
    }

    fn delete_resource_entity(&self, entity: &ResourceEntityRef) -> PortResult<()> {
        route!(self, |ports| ports.delete_resource_entity(entity))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::actor::model::trx::TrxWrapper;
    use crate::core::actor::model::trx::tests::{StubCore, StubStorage};
    use crate::models::ports::storage::IStorage;
    use std::sync::Arc;

    #[test]
    fn legacy_entities_pass_the_entity_conformance_suite() {
        let storage: Arc<dyn IStorage> = StubStorage::new();
        let trx = TrxWrapper::new(
            Arc::new(StubCore {
                storage: storage.clone(),
            }),
            storage,
            false,
        );
        Program {
            id: "13@conformance".to_owned(),
            machine_id: "1@global".to_owned(),
            ..Default::default()
        }
        .push(&*trx);
        let blobs = StorageRootBlobStore::new("/var/aseman");
        let entities = LegacyEntities {
            trx: &*trx,
            blobs: &blobs,
        };
        aseman_ports::conformance::entity_directory(&entities, "13@conformance");
        trx.put_json(
            &resource_store_key("vs-conformance"),
            "core",
            &serde_json::json!({"id": "vs-conformance", "name": "s", "machineId": "1@global"}),
            true,
        )
        .unwrap();
        aseman_ports::conformance::vm_resource_entities(&entities, "vs-conformance");
        // The legacy encodings: files by local path, the derived runtime link.
        assert_eq!(
            trx.get_link("vmEntityPath::13@conformance::main"),
            "/var/aseman/machines/13@conformance/entities/main/index.js"
        );
        assert_eq!(
            trx.get_link("vmEntityType::13@conformance::main"),
            "javascript"
        );
    }
}
