//! The entity ports of one state action (ADR 0036): entities, their artifacts and
//! configuration, and VM resource entities through the capsule repositories over the
//! action's transaction.

use aseman_domain::blob::BlobEvidence;
use aseman_domain::program::{
    ArtifactRole, EntityArtifact, EntityRecord, ResourceEntityRef, VmResourceEntity,
};
use aseman_ports::{BlobStore, EntityDirectory, PortError, PortResult, VmResourceEntities};

use crate::adapters::blob_store::StorageRootBlobStore;
use crate::api::model::program_ports::resource_store_key;
use crate::api::model::{Entity, Program};
use crate::core::trx::Trx;

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


/// The entity ports of one state action, routed per ADR 0026 to the action's
/// PostgreSQL unit of work when the node runs on PostgreSQL, else to legacy.
pub(crate) struct EntityPorts<'a> {
    pub(crate) trx: &'a Trx,
    /// Where the node keeps file bytes; legacy records files by local path.
    pub(crate) blobs: &'a StorageRootBlobStore,
}

/// Run `$call` on the adapter for the current provider, bound as `$ports`.
macro_rules! route {
    ($self:ident, |$ports:ident| $call:expr) => {{
        let $ports = aseman_capsule::entity::CapsuleEntityPorts { repository: $self.trx };
        $call
    }};
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

    #[test]
    fn entities_pass_the_entity_conformance_suites() {
        let trx = crate::core::trx::test_trx();
        let blobs = StorageRootBlobStore::new("/var/aseman");
        let entities = EntityPorts {
            trx: &trx,
            blobs: &blobs,
        };
        aseman_ports::conformance::entity_directory(&entities, "13@conformance");
        crate::api::model::program_ports::ProgramPorts { trx: &trx }
            .put_resource_store("vs-conformance", "s", "1@global", "{}")
            .unwrap();
        aseman_ports::conformance::vm_resource_entities(&entities, "vs-conformance");
    }
}
