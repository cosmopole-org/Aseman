//! Workload placement and guest database bindings on the capsule protocol (A405):
//! the trusted records the guest gateway resolves from.
//!
//! A workload (`core.workload`) relates to its program and creature, and the chain is
//! consistent only when the program belongs to the same creature; anything else fails
//! closed. A creature has one guest database binding (`core.guest_database_binding`),
//! keyed by the creature, whose generation never moves backwards.

use crate::store::{body, next_revision, port_error};
use crate::support::{Capsules, MAX_CAS_ATTEMPTS, failed, new_capsule, relationship, text};
use crate::{CapsuleStore, CapsuleStoreError};
use aseman_contracts::capsule::{CapsuleEnvelope, CapsuleValue, OwnerScope, StorageClass};
use aseman_domain::{
    BindingStatus, CreatureDatabaseBinding, CreatureId, DesiredWorkload, DesiredWorkloadState,
    Generation, ProgramId, Uuid, WorkloadId,
};
use aseman_ports::guest::LegacyWorkloadRefs;
use aseman_ports::{CreatureDatabaseBindings, PortError, PortResult, WorkloadRepository};
use std::collections::BTreeMap;

const WORKLOAD: &str = "core.workload";
const PROGRAM: &str = "core.program";
const CREATURE: &str = "core.creature";
const BINDING: &str = "core.guest_database_binding";

/// Workloads and bindings over any [`CapsuleStore`].
pub struct CapsuleWorkloads<'a> {
    pub repository: &'a dyn CapsuleStore,
}

fn state_name(state: DesiredWorkloadState) -> &'static str {
    match state {
        DesiredWorkloadState::Stopped => "stopped",
        DesiredWorkloadState::Running => "running",
        DesiredWorkloadState::Paused => "paused",
        DesiredWorkloadState::Deleted => "deleted",
    }
}

fn state(name: &str) -> PortResult<DesiredWorkloadState> {
    Ok(match name {
        "stopped" => DesiredWorkloadState::Stopped,
        "running" => DesiredWorkloadState::Running,
        "paused" => DesiredWorkloadState::Paused,
        "deleted" => DesiredWorkloadState::Deleted,
        other => return Err(failed(format!("unknown workload state {other}"))),
    })
}

fn integer(fields: &BTreeMap<String, CapsuleValue>, name: &str) -> PortResult<u64> {
    match fields.get(name) {
        Some(CapsuleValue::Integer(value)) => u64::try_from(*value).map_err(failed),
        _ => Err(failed(format!("{name} is missing"))),
    }
}

fn target(capsule: &CapsuleEnvelope, name: &str) -> PortResult<[u8; 16]> {
    capsule
        .relationships
        .iter()
        .find(|relationship| relationship.name == name)
        .map(|relationship| relationship.target_id.0)
        .ok_or_else(|| failed(format!("{} has no {name}", capsule.kind.0)))
}

impl WorkloadRepository for CapsuleWorkloads<'_> {
    fn create_desired(&self, workload: &DesiredWorkload) -> PortResult<()> {
        let creature = *workload.creature_id.as_uuid().as_bytes();
        let program = *workload.program_id.as_uuid().as_bytes();
        let program_capsule = Capsules(self.repository)
            .live(PROGRAM, program)?
            .ok_or_else(|| failed("the workload's program does not exist"))?;
        if target(&program_capsule, "creature")? != creature {
            return Err(failed("the workload's program belongs to another creature"));
        }
        let fields = BTreeMap::from([
            (
                "workload_name".to_owned(),
                CapsuleValue::Text(workload.name.clone()),
            ),
            (
                "runtime".to_owned(),
                CapsuleValue::Text(workload.runtime.clone()),
            ),
            (
                "desired_state".to_owned(),
                CapsuleValue::Text(state_name(workload.state).to_owned()),
            ),
            (
                "desired_generation".to_owned(),
                CapsuleValue::Integer(i64::try_from(workload.generation.get()).map_err(failed)?),
            ),
        ]);
        let capsule = new_capsule(
            *workload.id.as_uuid().as_bytes(),
            WORKLOAD,
            StorageClass::Core,
            OwnerScope::Creature(creature),
            vec![
                relationship("program", PROGRAM, program),
                relationship("creature", CREATURE, creature),
            ],
            fields,
        )?;
        self.repository.put(&capsule, None).map_err(port_error)
    }

    fn get_desired(&self, id: WorkloadId) -> PortResult<Option<DesiredWorkload>> {
        let capsules = Capsules(self.repository);
        let Some(capsule) = capsules.live(WORKLOAD, *id.as_uuid().as_bytes())? else {
            return Ok(None);
        };
        let fields = body(&capsule).ok_or(PortError::NotFound)?;
        let creature = target(&capsule, "creature")?;
        let program = target(&capsule, "program")?;
        // The chain holds only when the program belongs to the workload's creature.
        let program_capsule = capsules
            .live(PROGRAM, program)?
            .ok_or_else(|| failed("the workload's program does not exist"))?;
        if target(&program_capsule, "creature")? != creature {
            return Err(failed("the workload's program belongs to another creature"));
        }
        Ok(Some(DesiredWorkload {
            id,
            creature_id: CreatureId::from_uuid(Uuid::from_bytes(creature)),
            program_id: ProgramId::from_uuid(Uuid::from_bytes(program)),
            name: text(fields, "workload_name"),
            runtime: text(fields, "runtime"),
            generation: Generation::from_stored(integer(fields, "desired_generation")?)
                .map_err(failed)?,
            state: state(&text(fields, "desired_state"))?,
        }))
    }

    fn put_desired(&self, workload: &DesiredWorkload, expected: Generation) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let current = Capsules(self.repository)
                .live(WORKLOAD, *workload.id.as_uuid().as_bytes())?
                .ok_or(PortError::NotFound)?;
            let mut fields = body(&current).ok_or(PortError::NotFound)?.clone();
            if integer(&fields, "desired_generation")? != expected.get() {
                return Err(PortError::Conflict);
            }
            fields.insert(
                "desired_state".to_owned(),
                CapsuleValue::Text(state_name(workload.state).to_owned()),
            );
            fields.insert(
                "desired_generation".to_owned(),
                CapsuleValue::Integer(i64::try_from(workload.generation.get()).map_err(failed)?),
            );
            match self
                .repository
                .put(&next_revision(&current, fields)?, Some(current.revision))
            {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }
}

impl LegacyWorkloadRefs for CapsuleWorkloads<'_> {
    fn legacy_refs(&self, workload: &DesiredWorkload) -> PortResult<(String, String)> {
        let capsules = Capsules(self.repository);
        Ok((
            capsules.legacy_id_of(CREATURE, *workload.creature_id.as_uuid().as_bytes())?,
            capsules.legacy_id_of(PROGRAM, *workload.program_id.as_uuid().as_bytes())?,
        ))
    }
}

fn status_name(status: BindingStatus) -> &'static str {
    match status {
        BindingStatus::Disabled => "disabled",
        BindingStatus::Active => "active",
    }
}

impl CreatureDatabaseBindings for CapsuleWorkloads<'_> {
    fn binding_for(&self, creature: CreatureId) -> PortResult<Option<CreatureDatabaseBinding>> {
        let Some(capsule) =
            Capsules(self.repository).live(BINDING, *creature.as_uuid().as_bytes())?
        else {
            return Ok(None);
        };
        let fields = body(&capsule).ok_or(PortError::NotFound)?;
        let mut binding = CreatureDatabaseBinding::new(
            creature,
            text(fields, "provider_id"),
            text(fields, "database_name"),
            text(fields, "role_name"),
        )
        .map_err(failed)?;
        binding.generation =
            Generation::from_stored(integer(fields, "generation")?).map_err(failed)?;
        binding.status = match text(fields, "status").as_str() {
            "active" => BindingStatus::Active,
            "disabled" => BindingStatus::Disabled,
            other => return Err(failed(format!("unknown binding status {other}"))),
        };
        Ok(Some(binding))
    }

    fn record_binding(&self, binding: &CreatureDatabaseBinding) -> PortResult<()> {
        let id = *binding.creature_id.as_uuid().as_bytes();
        let fields = |catalog_revision: i64| -> PortResult<BTreeMap<String, CapsuleValue>> {
            Ok(BTreeMap::from([
                (
                    "provider_id".to_owned(),
                    CapsuleValue::Text(binding.provider_id.clone()),
                ),
                (
                    "database_name".to_owned(),
                    CapsuleValue::Text(binding.database.clone()),
                ),
                (
                    "role_name".to_owned(),
                    CapsuleValue::Text(binding.role.clone()),
                ),
                (
                    "generation".to_owned(),
                    CapsuleValue::Integer(i64::try_from(binding.generation.get()).map_err(failed)?),
                ),
                (
                    "schema_catalog_revision".to_owned(),
                    CapsuleValue::Integer(catalog_revision),
                ),
                (
                    "status".to_owned(),
                    CapsuleValue::Text(status_name(binding.status).to_owned()),
                ),
            ]))
        };
        for _ in 0..MAX_CAS_ATTEMPTS {
            let written = match Capsules(self.repository).get(BINDING, id)? {
                Some(current) => {
                    let stored = body(&current).cloned().unwrap_or_default();
                    if !current.tombstone
                        && integer(&stored, "generation")? > binding.generation.get()
                    {
                        return Err(PortError::Conflict);
                    }
                    // The schema catalog revision belongs to the provider; keep it.
                    let revision = match stored.get("schema_catalog_revision") {
                        Some(CapsuleValue::Integer(revision)) => *revision,
                        _ => 0,
                    };
                    self.repository.put(
                        &next_revision(&current, fields(revision)?)?,
                        Some(current.revision),
                    )
                }
                None => self.repository.put(
                    &new_capsule(
                        id,
                        BINDING,
                        StorageClass::Core,
                        OwnerScope::Creature(id),
                        vec![relationship("creature", CREATURE, id)],
                        fields(0)?,
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
}
