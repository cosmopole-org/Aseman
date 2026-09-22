//! Program repositories on the capsule protocol (RL-004 strangler, target side).
//!
//! A program is stored exactly as the A308 export writes it: a `core.program` capsule
//! scoped to its machine creature, related to it, with a `core.legacy_identity` row.
//! A machine may own several programs.

use crate::store::{body, next_revision, port_error};
use crate::support::{
    Capsules, DocumentFamily, MAX_CAS_ATTEMPTS, equal, failed, legacy_identity, new_capsule,
    relationship, text, tombstone,
};
use crate::{CapsuleStore, CapsuleStoreError};
use aseman_contracts::capsule::{
    CapsuleEnvelope, CapsuleKind, CapsuleQuery, CapsuleValue, MAX_QUERY_LIMIT, OwnerScope,
    StorageClass,
};
use aseman_contracts::legacy_documents::{
    capsule_value_to_json, legacy_document_fields, merge_legacy_objects,
};
use aseman_contracts::legacy_realtime::deterministic_legacy_capsule_id;
use aseman_domain::creature::legacy_page;
use aseman_domain::program::{ProgramAlarm, ProgramRecord, VmResourceStore};
use aseman_ports::{
    PortError, PortResult, ProgramAlarms, ProgramDirectory, ProgramMetadata, VmResourceStores,
};
use std::collections::{BTreeMap, BTreeSet};

const PROGRAM: &str = "core.program";
const CREATURE: &str = "core.creature";

/// Program ports over any [`CapsuleStore`].
pub struct CapsuleProgramPorts<'a> {
    pub repository: &'a dyn CapsuleStore,
}

fn program_id(legacy_id: &str) -> [u8; 16] {
    deterministic_legacy_capsule_id("Program", legacy_id.as_bytes())
}

fn machine_capsule_id(machine_id: &str) -> [u8; 16] {
    deterministic_legacy_capsule_id("Creature", machine_id.as_bytes())
}

fn fields(record: &ProgramRecord) -> BTreeMap<String, CapsuleValue> {
    BTreeMap::from([
        (
            "machine_id".to_owned(),
            CapsuleValue::Text(record.machine_id.clone()),
        ),
        (
            "runtime".to_owned(),
            CapsuleValue::Text(record.runtime.clone()),
        ),
        ("path".to_owned(), CapsuleValue::Text(record.path.clone())),
        (
            "comment".to_owned(),
            CapsuleValue::Text(record.comment.clone()),
        ),
    ])
}

fn record(legacy_id: &str, capsule: &CapsuleEnvelope) -> PortResult<ProgramRecord> {
    let fields = body(capsule).ok_or(PortError::NotFound)?;
    Ok(ProgramRecord {
        id: legacy_id.to_owned(),
        machine_id: text(fields, "machine_id"),
        runtime: text(fields, "runtime"),
        path: text(fields, "path"),
        comment: text(fields, "comment"),
    })
}

impl CapsuleProgramPorts<'_> {
    fn capsules(&self) -> Capsules<'_> {
        Capsules(self.repository)
    }

    /// The machine a program is scoped to must exist, as in the A308 export.
    fn machine_scope(&self, record: &ProgramRecord) -> PortResult<[u8; 16]> {
        let machine = machine_capsule_id(&record.machine_id);
        if self.capsules().live(CREATURE, machine)?.is_none() {
            return Err(failed(format!(
                "program machine {} does not exist",
                record.machine_id
            )));
        }
        Ok(machine)
    }
}

impl ProgramDirectory for CapsuleProgramPorts<'_> {
    fn program(&self, legacy_id: &str) -> PortResult<Option<ProgramRecord>> {
        self.capsules()
            .live(PROGRAM, program_id(legacy_id))?
            .map(|capsule| record(legacy_id, &capsule))
            .transpose()
    }

    fn programs(&self, offset: i64, count: Option<i64>) -> PortResult<Vec<ProgramRecord>> {
        // Legacy lists objects in identity byte order.
        let mut identities = self
            .capsules()
            .legacy_ids("Program")?
            .into_values()
            .collect::<Vec<_>>();
        identities.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        let mut records = Vec::new();
        for legacy_id in identities {
            if let Some(program) = self.program(&legacy_id)? {
                records.push(program);
            }
        }
        Ok(legacy_page(records, offset, count))
    }

    fn programs_of_machine(&self, machine_id: &str) -> PortResult<Vec<ProgramRecord>> {
        let rows = self
            .repository
            .query(&CapsuleQuery {
                kind: CapsuleKind(PROGRAM.to_owned()),
                predicate: Some(equal(
                    "machine_id",
                    CapsuleValue::Text(machine_id.to_owned()),
                )),
                projection: BTreeSet::new(),
                sort: Vec::new(),
                aggregates: Vec::new(),
                traversals: Vec::new(),
                limit: MAX_QUERY_LIMIT,
                cursor: None,
            })
            .map_err(port_error)?;
        let mut records = Vec::new();
        for capsule in rows.iter().filter(|capsule| !capsule.tombstone) {
            let legacy_id = self.capsules().legacy_id_of(PROGRAM, capsule.id.0)?;
            records.push(record(&legacy_id, capsule)?);
        }
        records.sort_by(|left, right| left.id.as_bytes().cmp(right.id.as_bytes()));
        Ok(records)
    }

    fn create_program(&self, program: &ProgramRecord) -> PortResult<()> {
        let id = program_id(&program.id);
        let existing = self.capsules().get(PROGRAM, id)?;
        if existing.as_ref().is_some_and(|capsule| !capsule.tombstone) {
            return Err(PortError::Conflict);
        }
        let machine = self.machine_scope(program)?;
        let writes = match existing {
            // Registering a deleted program again revives it, as legacy allows.
            Some(tombstoned) => vec![(
                CapsuleEnvelope {
                    owner_scope: OwnerScope::Creature(machine),
                    relationships: vec![relationship("creature", CREATURE, machine)],
                    ..next_revision(&tombstoned, fields(program))?
                }
                .seal()
                .map_err(failed)?,
                Some(tombstoned.revision),
            )],
            None => vec![
                (
                    new_capsule(
                        id,
                        PROGRAM,
                        StorageClass::Core,
                        OwnerScope::Creature(machine),
                        vec![relationship("creature", CREATURE, machine)],
                        fields(program),
                    )?,
                    None,
                ),
                (legacy_identity("Program", &program.id, PROGRAM)?, None),
            ],
        };
        self.repository.put_all(&writes).map_err(port_error)
    }

    fn update_program(&self, program: &ProgramRecord) -> PortResult<()> {
        let machine = self.machine_scope(program)?;
        for _ in 0..MAX_CAS_ATTEMPTS {
            let current = self
                .capsules()
                .live(PROGRAM, program_id(&program.id))?
                .ok_or(PortError::NotFound)?;
            let next = CapsuleEnvelope {
                owner_scope: OwnerScope::Creature(machine),
                relationships: vec![relationship("creature", CREATURE, machine)],
                ..next_revision(&current, fields(program))?
            }
            .seal()
            .map_err(failed)?;
            match self.repository.put(&next, Some(current.revision)) {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }

    fn delete_program(&self, legacy_id: &str) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some(current) = self.capsules().live(PROGRAM, program_id(legacy_id))? else {
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

const PROGRAM_METADATA: DocumentFamily = DocumentFamily {
    kind: "core.program_metadata",
    family: "ProgramMetadata",
    key_prefix: "ProgMeta::",
    subject_relationship: "program",
    subject_kind: PROGRAM,
    subject_family: "Program",
    root: "metadata",
};

impl ProgramMetadata for CapsuleProgramPorts<'_> {
    fn program_metadata(&self, legacy_id: &str, path: &str) -> PortResult<Option<String>> {
        self.capsules()
            .document_at(&PROGRAM_METADATA, legacy_id, path)
    }

    fn merge_program_metadata(&self, legacy_id: &str, document: &str) -> PortResult<()> {
        self.capsules()
            .merge_document(&PROGRAM_METADATA, legacy_id, document)
    }

    fn delete_program_metadata(&self, legacy_id: &str) -> PortResult<()> {
        self.capsules()
            .delete_document(&PROGRAM_METADATA, legacy_id)
    }
}

const PROGRAM_ALARM: &str = "core.program_alarm";
const STORE: &str = "core.store";

fn alarm_id(legacy_id: &str) -> [u8; 16] {
    deterministic_legacy_capsule_id("ProgramAlarm", legacy_id.as_bytes())
}

impl ProgramAlarms for CapsuleProgramPorts<'_> {
    fn alarm(&self, legacy_id: &str) -> PortResult<Option<ProgramAlarm>> {
        let Some(capsule) = self.capsules().live(PROGRAM_ALARM, alarm_id(legacy_id))? else {
            return Ok(None);
        };
        let fields = body(&capsule).ok_or(PortError::NotFound)?;
        let store = capsule
            .relationships
            .iter()
            .find(|relationship| relationship.name == "store")
            .ok_or_else(|| failed("program alarm has no store"))?;
        let fire_at_micros = match fields.get("fire_at_micros") {
            Some(CapsuleValue::Integer(micros)) => *micros,
            _ => return Err(failed("program alarm has no fire time")),
        };
        Ok(Some(ProgramAlarm {
            store_id: self.capsules().legacy_id_of(STORE, store.target_id.0)?,
            fire_at_millis: fire_at_micros / 1_000,
            data: text(fields, "data"),
            entity: text(fields, "entity_name"),
        }))
    }

    fn set_alarm(&self, legacy_id: &str, alarm: &ProgramAlarm) -> PortResult<()> {
        let program = self
            .capsules()
            .live(PROGRAM, program_id(legacy_id))?
            .ok_or(PortError::NotFound)?;
        let fire_at_micros = alarm
            .fire_at_millis
            .checked_mul(1_000)
            .ok_or_else(|| failed("alarm time overflows microseconds"))?;
        let relationships = vec![
            relationship("program", PROGRAM, program_id(legacy_id)),
            relationship(
                "store",
                STORE,
                deterministic_legacy_capsule_id("Store", alarm.store_id.as_bytes()),
            ),
        ];
        let fields = BTreeMap::from([
            (
                "fire_at_micros".to_owned(),
                CapsuleValue::Integer(fire_at_micros),
            ),
            (
                "entity_name".to_owned(),
                CapsuleValue::Text(alarm.entity.clone()),
            ),
            ("data".to_owned(), CapsuleValue::Text(alarm.data.clone())),
        ]);
        let id = alarm_id(legacy_id);
        for _ in 0..MAX_CAS_ATTEMPTS {
            let written = match self.capsules().get(PROGRAM_ALARM, id)? {
                Some(current) => {
                    let next = CapsuleEnvelope {
                        relationships: relationships.clone(),
                        ..next_revision(&current, fields.clone())?
                    }
                    .seal()
                    .map_err(failed)?;
                    self.repository.put(&next, Some(current.revision))
                }
                None => self.repository.put(
                    &new_capsule(
                        id,
                        PROGRAM_ALARM,
                        StorageClass::Core,
                        program.owner_scope.clone(),
                        relationships.clone(),
                        fields.clone(),
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

    fn clear_alarm(&self, legacy_id: &str) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some(current) = self.capsules().live(PROGRAM_ALARM, alarm_id(legacy_id))? else {
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

const RESOURCE_STORE: &str = "core.vm_resource_store";

fn resource_store_id(store_id: &str) -> [u8; 16] {
    deterministic_legacy_capsule_id("VmResourceStore", store_id.as_bytes())
}

impl CapsuleProgramPorts<'_> {
    /// The creature that owns a machine reference: the machine creature itself, or
    /// the machine of a program (as the A308 export resolves it).
    fn machine_owner(&self, machine_id: &str) -> PortResult<[u8; 16]> {
        let creature = machine_capsule_id(machine_id);
        if self.capsules().live(CREATURE, creature)?.is_some() {
            return Ok(creature);
        }
        match self.program(machine_id)? {
            Some(program) => Ok(machine_capsule_id(&program.machine_id)),
            None => Err(failed(format!(
                "machine {machine_id} names no creature or program"
            ))),
        }
    }
}

impl VmResourceStores for CapsuleProgramPorts<'_> {
    fn resource_store(&self, store_id: &str) -> PortResult<Option<VmResourceStore>> {
        let Some(capsule) = self
            .capsules()
            .live(RESOURCE_STORE, resource_store_id(store_id))?
        else {
            return Ok(None);
        };
        let fields = body(&capsule).ok_or(PortError::NotFound)?;
        let metadata = match fields.get("document").map(capsule_value_to_json) {
            Some(Ok(document @ serde_json::Value::Object(_))) => {
                serde_json::to_string(&document).map_err(failed)?
            }
            _ => return Err(failed("resource store has no metadata document")),
        };
        Ok(Some(VmResourceStore {
            id: store_id.to_owned(),
            name: text(fields, "name"),
            machine_id: text(fields, "machine_ref"),
            metadata,
        }))
    }

    fn resource_stores(&self, machine_id: Option<&str>) -> PortResult<Vec<String>> {
        let mut stores = match machine_id {
            Some(machine) => {
                let rows = self
                    .repository
                    .query(&CapsuleQuery {
                        kind: CapsuleKind(RESOURCE_STORE.to_owned()),
                        predicate: Some(equal(
                            "machine_ref",
                            CapsuleValue::Text(machine.to_owned()),
                        )),
                        projection: BTreeSet::new(),
                        sort: Vec::new(),
                        aggregates: Vec::new(),
                        traversals: Vec::new(),
                        limit: MAX_QUERY_LIMIT,
                        cursor: None,
                    })
                    .map_err(port_error)?;
                rows.iter()
                    .filter(|capsule| !capsule.tombstone)
                    .map(|capsule| self.capsules().legacy_id_of(RESOURCE_STORE, capsule.id.0))
                    .collect::<PortResult<Vec<_>>>()?
            }
            None => {
                let mut live = Vec::new();
                for (id, legacy_id) in self.capsules().legacy_ids("VmResourceStore")? {
                    if self.capsules().live(RESOURCE_STORE, id)?.is_some() {
                        live.push(legacy_id);
                    }
                }
                live
            }
        };
        stores.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
        Ok(stores)
    }

    fn put_resource_store(
        &self,
        store_id: &str,
        name: &str,
        machine_id: &str,
        metadata: &str,
    ) -> PortResult<()> {
        let Ok(serde_json::Value::Object(incoming)) = serde_json::from_str(metadata) else {
            return Err(failed("metadata must be a JSON object"));
        };
        let id = resource_store_id(store_id);
        let key = format!("Json::VmResourceStore::{store_id}");
        for _ in 0..MAX_CAS_ATTEMPTS {
            let current = self.capsules().get(RESOURCE_STORE, id)?;
            let live = current.as_ref().filter(|capsule| !capsule.tombstone);
            // LD-21: an update without a machine keeps the owner.
            let machine = if machine_id.is_empty() {
                live.and_then(body)
                    .map(|fields| text(fields, "machine_ref"))
                    .filter(|machine| !machine.is_empty())
                    .ok_or_else(|| failed("a resource store needs a machine"))?
            } else {
                machine_id.to_owned()
            };
            let mut document = match live
                .and_then(body)
                .and_then(|fields| fields.get("document"))
                .map(capsule_value_to_json)
            {
                Some(Ok(serde_json::Value::Object(document))) => document,
                _ => serde_json::Map::new(),
            };
            merge_legacy_objects(&mut document, &incoming);
            let mut fields = legacy_document_fields(&key, "metadata", &document).map_err(failed)?;
            fields.insert("name".to_owned(), CapsuleValue::Text(name.to_owned()));
            fields.insert(
                "machine_ref".to_owned(),
                CapsuleValue::Text(machine.clone()),
            );
            let owner = self.machine_owner(&machine)?;
            let relationships = vec![relationship("creature", CREATURE, owner)];
            let written = match current {
                Some(current) => {
                    let next = CapsuleEnvelope {
                        owner_scope: OwnerScope::Creature(owner),
                        relationships,
                        ..next_revision(&current, fields)?
                    }
                    .seal()
                    .map_err(failed)?;
                    self.repository.put(&next, Some(current.revision))
                }
                None => self.repository.put_all(&[
                    (
                        new_capsule(
                            id,
                            RESOURCE_STORE,
                            StorageClass::Core,
                            OwnerScope::Creature(owner),
                            relationships,
                            fields,
                        )?,
                        None,
                    ),
                    (
                        legacy_identity("VmResourceStore", store_id, RESOURCE_STORE)?,
                        None,
                    ),
                ]),
            };
            match written {
                Err(CapsuleStoreError::Conflict) => continue,
                other => return other.map_err(port_error),
            }
        }
        Err(PortError::Conflict)
    }

    fn delete_resource_store(&self, store_id: &str) -> PortResult<()> {
        for _ in 0..MAX_CAS_ATTEMPTS {
            let Some(current) = self
                .capsules()
                .live(RESOURCE_STORE, resource_store_id(store_id))?
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
