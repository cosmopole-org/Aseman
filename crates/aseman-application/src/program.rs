//! Program use cases (RL-004 strangler slice: `/programs/create`, `update`, `delete`,
//! and the entity records every deploy path writes).
//! Only the owner of a program's machine may change the program (LD-18); legacy
//! checked ownership on create only. Error texts are the legacy ones.

use crate::ApplicationError;
use aseman_domain::blob::BlobEvidence;
use aseman_domain::program::{ArtifactRole, EntityRecord, ProgramRecord, ResourceEntityRef};
use aseman_ports::{
    BlobStore, CreatureDirectory, EntityDirectory, PortError, ProgramDirectory, VmResourceEntities,
};

fn denied(message: &str) -> ApplicationError {
    ApplicationError::Denied(message.to_owned())
}

/// The machine a program belongs to, provided `caller_id` owns it.
fn owned_machine(
    creatures: &dyn CreatureDirectory,
    machine_id: &str,
    caller_id: &str,
) -> Result<(), ApplicationError> {
    let machine = creatures
        .creature(machine_id)?
        .ok_or_else(|| denied("machine not found"))?;
    if machine.owner_id != caller_id {
        return Err(denied("you are not owner of machine"));
    }
    Ok(())
}

/// What `/programs/create` asks for.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NewProgram {
    /// The server-generated identity.
    pub id: String,
    pub machine_id: String,
    pub runtime: String,
    pub path: String,
    pub comment: String,
}

pub struct CreateProgram<'a> {
    pub creatures: &'a dyn CreatureDirectory,
    pub programs: &'a dyn ProgramDirectory,
}

impl CreateProgram<'_> {
    pub fn execute(
        &self,
        caller_id: &str,
        request: NewProgram,
    ) -> Result<ProgramRecord, ApplicationError> {
        owned_machine(self.creatures, &request.machine_id, caller_id)?;
        let record = ProgramRecord {
            id: request.id,
            machine_id: request.machine_id,
            runtime: request.runtime,
            path: request.path,
            comment: request.comment,
        };
        match self.programs.create_program(&record) {
            Err(PortError::Conflict) => Err(denied("program already exists")),
            other => other.map_err(ApplicationError::from),
        }?;
        Ok(record)
    }
}

/// The program `caller_id` may change, after the LD-18 ownership check.
fn owned_program(
    creatures: &dyn CreatureDirectory,
    programs: &dyn ProgramDirectory,
    caller_id: &str,
    program_id: &str,
) -> Result<ProgramRecord, ApplicationError> {
    let program = programs
        .program(program_id)?
        .ok_or_else(|| denied("program does not exist"))?;
    owned_machine(creatures, &program.machine_id, caller_id)?;
    Ok(program)
}

pub struct UpdateProgramPath<'a> {
    pub creatures: &'a dyn CreatureDirectory,
    pub programs: &'a dyn ProgramDirectory,
}

impl UpdateProgramPath<'_> {
    /// Legacy `/programs/update` changes only the path (and merges metadata, which the
    /// adapter applies through the metadata port).
    pub fn execute(
        &self,
        caller_id: &str,
        program_id: &str,
        path: &str,
    ) -> Result<ProgramRecord, ApplicationError> {
        let mut program = owned_program(self.creatures, self.programs, caller_id, program_id)?;
        program.path = path.to_owned();
        self.programs.update_program(&program)?;
        Ok(program)
    }
}

pub struct DeleteProgram<'a> {
    pub creatures: &'a dyn CreatureDirectory,
    pub programs: &'a dyn ProgramDirectory,
}

impl DeleteProgram<'_> {
    pub fn execute(&self, caller_id: &str, program_id: &str) -> Result<(), ApplicationError> {
        owned_program(self.creatures, self.programs, caller_id, program_id)?;
        self.programs.delete_program(program_id)?;
        Ok(())
    }
}

/// A deployed entity whose files are already stored (ADR 0027).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EntityDeployment {
    pub entity: EntityRecord,
    /// The stored primary file.
    pub primary: BlobEvidence,
    /// Record the primary file for runtimes that open it by path.
    pub runtime_file: bool,
    /// Let clients fetch the primary file (`/programs/downloadEntity`).
    pub downloadable: bool,
    /// Configuration to merge into the entity's document (proxy entities).
    pub config: Option<String>,
}

/// Record a deployment: the entity, then its files and configuration. It is the one
/// write shared by `/programs/deploy`, the VM deploy host call, and the cluster
/// applier.
pub struct RecordEntityDeployment<'a> {
    pub entities: &'a dyn EntityDirectory,
}

impl RecordEntityDeployment<'_> {
    pub fn execute(&self, deployment: &EntityDeployment) -> Result<(), ApplicationError> {
        let entity = &deployment.entity;
        match self.entities.put_entity(entity) {
            Err(PortError::NotFound) => return Err(denied("program not found")),
            other => other?,
        }
        let roles = [
            (deployment.runtime_file, ArtifactRole::Primary),
            (deployment.downloadable, ArtifactRole::Downloadable),
        ];
        for (_, role) in roles.iter().filter(|(wanted, _)| *wanted) {
            self.entities.put_artifact(
                &entity.program_id,
                &entity.entity_id,
                *role,
                &deployment.primary,
            )?;
        }
        if let Some(config) = &deployment.config {
            self.entities
                .merge_entity_config(&entity.program_id, &entity.entity_id, config)?;
        }
        Ok(())
    }
}

/// `createResourceEntity`: store the entity's data, then record it. The reference is
/// checked before any file is written (LD-26).
pub struct PutResourceEntity<'a> {
    pub entities: &'a dyn VmResourceEntities,
    pub blobs: &'a dyn BlobStore,
}

impl PutResourceEntity<'_> {
    pub fn execute(
        &self,
        entity: &ResourceEntityRef,
        payload: &str,
        data: &[u8],
    ) -> Result<(), ApplicationError> {
        if !entity.is_valid() {
            return Err(denied("invalid resource entity"));
        }
        let evidence = self
            .blobs
            .put_blob(&entity.data_key(), data, "application/json", true)?;
        match self
            .entities
            .put_resource_entity(entity, payload, &evidence)
        {
            Err(PortError::NotFound) => {
                self.blobs.delete_blob(&evidence.store_key)?;
                Err(denied("resource store not found"))
            }
            other => Ok(other?),
        }
    }
}

/// `deleteResourceEntity`: remove the record and its data.
pub struct DeleteResourceEntity<'a> {
    pub entities: &'a dyn VmResourceEntities,
    pub blobs: &'a dyn BlobStore,
}

impl DeleteResourceEntity<'_> {
    pub fn execute(&self, entity: &ResourceEntityRef) -> Result<(), ApplicationError> {
        if !entity.is_valid() {
            return Err(denied("invalid resource entity"));
        }
        let data_key = self
            .entities
            .resource_entity(entity)?
            .and_then(|stored| stored.data_key);
        self.entities.delete_resource_entity(entity)?;
        if let Some(key) = data_key {
            self.blobs.delete_blob(&key)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_domain::program::EntityArtifact;
    use aseman_ports::PortResult;
    use std::sync::Mutex;

    /// Records the writes in order; only program `p` exists.
    #[derive(Default)]
    struct Recorded(Mutex<Vec<String>>);

    impl EntityDirectory for Recorded {
        fn entity(&self, _: &str, _: &str) -> PortResult<Option<EntityRecord>> {
            Ok(None)
        }
        fn put_entity(&self, entity: &EntityRecord) -> PortResult<()> {
            if entity.program_id != "p" {
                return Err(PortError::NotFound);
            }
            self.0
                .lock()
                .unwrap()
                .push(format!("entity {}", entity.entity_type));
            Ok(())
        }
        fn artifact(
            &self,
            _: &str,
            _: &str,
            _: ArtifactRole,
        ) -> PortResult<Option<EntityArtifact>> {
            Ok(None)
        }
        fn put_artifact(
            &self,
            _: &str,
            _: &str,
            role: ArtifactRole,
            evidence: &BlobEvidence,
        ) -> PortResult<()> {
            self.0
                .lock()
                .unwrap()
                .push(format!("{} {}", role.as_str(), evidence.store_key));
            Ok(())
        }
        fn deployed_programs(&self) -> PortResult<Vec<String>> {
            Ok(Vec::new())
        }
        fn entity_config(&self, _: &str, _: &str) -> PortResult<Option<String>> {
            Ok(None)
        }
        fn merge_entity_config(&self, _: &str, _: &str, document: &str) -> PortResult<()> {
            self.0.lock().unwrap().push(format!("config {document}"));
            Ok(())
        }
    }

    fn deployment(program_id: &str) -> EntityDeployment {
        EntityDeployment {
            entity: EntityRecord {
                program_id: program_id.to_owned(),
                entity_id: "main".to_owned(),
                entity_type: "wasm".to_owned(),
                image_name: "main".to_owned(),
            },
            primary: BlobEvidence {
                store_key: "machines/p/entities/main/module.wasm".to_owned(),
                content_digest: [0; 32],
                size_bytes: 1,
                media_type: "application/octet-stream".to_owned(),
            },
            runtime_file: true,
            downloadable: false,
            config: None,
        }
    }

    #[test]
    fn a_deployment_writes_the_entity_before_its_files_and_config() {
        let entities = Recorded::default();
        let use_case = RecordEntityDeployment {
            entities: &entities,
        };
        use_case
            .execute(&EntityDeployment {
                downloadable: true,
                config: Some("{}".to_owned()),
                ..deployment("p")
            })
            .unwrap();
        assert_eq!(
            *entities.0.lock().unwrap(),
            [
                "entity wasm",
                "primary machines/p/entities/main/module.wasm",
                "downloadable machines/p/entities/main/module.wasm",
                "config {}",
            ]
        );
        // A runtime without links (docker) records only the entity.
        entities.0.lock().unwrap().clear();
        use_case
            .execute(&EntityDeployment {
                runtime_file: false,
                ..deployment("p")
            })
            .unwrap();
        assert_eq!(*entities.0.lock().unwrap(), ["entity wasm"]);
        assert_eq!(
            use_case.execute(&deployment("missing")),
            Err(ApplicationError::Denied("program not found".to_owned()))
        );
    }
    /// Blobs and resource entities in memory; only store `vs` exists.
    #[derive(Default)]
    struct Resources {
        blobs: Mutex<std::collections::BTreeMap<String, Vec<u8>>>,
        entities: Mutex<std::collections::BTreeMap<String, Option<String>>>,
    }

    impl BlobStore for Resources {
        fn put_blob(
            &self,
            key: &str,
            bytes: &[u8],
            media_type: &str,
            _: bool,
        ) -> PortResult<BlobEvidence> {
            self.blobs
                .lock()
                .unwrap()
                .insert(key.to_owned(), bytes.to_vec());
            Ok(BlobEvidence {
                store_key: key.to_owned(),
                content_digest: [0; 32],
                size_bytes: bytes.len() as u64,
                media_type: media_type.to_owned(),
            })
        }
        fn blob(&self, key: &str) -> PortResult<Option<Vec<u8>>> {
            Ok(self.blobs.lock().unwrap().get(key).cloned())
        }
        fn has_blob(&self, key: &str) -> PortResult<bool> {
            Ok(self.blobs.lock().unwrap().contains_key(key))
        }
        fn delete_blob(&self, key: &str) -> PortResult<()> {
            self.blobs.lock().unwrap().remove(key);
            Ok(())
        }
        fn local_path(&self, key: &str) -> PortResult<std::path::PathBuf> {
            Ok(key.into())
        }
    }

    impl VmResourceEntities for Resources {
        fn resource_entity(
            &self,
            entity: &ResourceEntityRef,
        ) -> PortResult<Option<aseman_domain::program::VmResourceEntity>> {
            Ok(self
                .entities
                .lock()
                .unwrap()
                .get(&entity.legacy_id())
                .map(|data_key| aseman_domain::program::VmResourceEntity {
                    reference: entity.clone(),
                    payload: "{}".to_owned(),
                    data_key: data_key.clone(),
                }))
        }
        fn put_resource_entity(
            &self,
            entity: &ResourceEntityRef,
            _: &str,
            data: &BlobEvidence,
        ) -> PortResult<()> {
            if entity.store_id != "vs" {
                return Err(PortError::NotFound);
            }
            self.entities
                .lock()
                .unwrap()
                .insert(entity.legacy_id(), Some(data.store_key.clone()));
            Ok(())
        }
        fn delete_resource_entity(&self, entity: &ResourceEntityRef) -> PortResult<()> {
            self.entities.lock().unwrap().remove(&entity.legacy_id());
            Ok(())
        }
    }

    #[test]
    fn resource_entities_keep_their_data_with_the_record() {
        let resources = Resources::default();
        let put = PutResourceEntity {
            entities: &resources,
            blobs: &resources,
        };
        let entity = |store: &str, kind: &str| ResourceEntityRef {
            store_id: store.to_owned(),
            entity_type: kind.to_owned(),
            entity_id: "e".to_owned(),
        };
        put.execute(&entity("vs", "doc"), "{}", b"hi").unwrap();
        assert_eq!(
            resources.blob("vm_stores/vs/doc/e.json"),
            Ok(Some(b"hi".to_vec()))
        );
        // LD-26: nothing is written for a traversing reference.
        assert_eq!(
            put.execute(&entity("vs", "../.."), "{}", b"x"),
            Err(ApplicationError::Denied(
                "invalid resource entity".to_owned()
            ))
        );
        // A missing store leaves no data behind.
        assert_eq!(
            put.execute(&entity("gone", "doc"), "{}", b"x"),
            Err(ApplicationError::Denied(
                "resource store not found".to_owned()
            ))
        );
        assert_eq!(resources.blobs.lock().unwrap().len(), 1);
        DeleteResourceEntity {
            entities: &resources,
            blobs: &resources,
        }
        .execute(&entity("vs", "doc"))
        .unwrap();
        assert!(resources.blobs.lock().unwrap().is_empty());
        assert!(resources.entities.lock().unwrap().is_empty());
    }
}
