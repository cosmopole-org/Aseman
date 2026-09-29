//! The entity ports of one state action (ADR 0036): entities, their artifacts and
//! configuration, and VM resource entities through the capsule repositories over the
//! action's transaction.

use aseman_domain::blob::BlobEvidence;
use aseman_domain::program::{
    ArtifactRole, EntityArtifact, EntityRecord, ResourceEntityRef, VmResourceEntity,
};
use aseman_ports::{EntityDirectory, PortResult, VmResourceEntities};

use crate::storage::Trx;

/// The entity ports of one state action.
pub(crate) struct EntityPorts<'a> {
    pub(crate) trx: &'a Trx,
}

/// Run `$call` on the adapter for the current provider, bound as `$ports`.
macro_rules! route {
    ($self:ident, |$ports:ident| $call:expr) => {{
        let $ports = aseman_capsule::entity::CapsuleEntityPorts {
            repository: $self.trx,
        };
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
