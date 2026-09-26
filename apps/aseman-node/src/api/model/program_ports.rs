//! Legacy adapter for the program use cases: the node transaction behind
//! [`ProgramDirectory`] (RL-004 strangler). Key encodings are exactly the legacy
//! `Program` object and its derived `machinePrograms::{machine}::{program}` link.

use aseman_domain::creature::legacy_page;
use aseman_domain::program::{DEFAULT_ALARM_ENTITY, ProgramAlarm, ProgramRecord, VmResourceStore};
use aseman_ports::{
    PortError, PortResult, ProgramAlarms, ProgramDirectory, ProgramMetadata, VmResourceStores,
};

use crate::models::transaction::ITrx;
use crate::shell::api::model::Program;

/// The legacy `Program` object columns, as `Program::push` writes them.
const PROGRAM_COLUMNS: [&str; 6] = ["|", "id", "machineId", "runtime", "path", "comment"];

/// The legacy adapter: the `Program` object, its `machinePrograms` link, `ProgMeta`,
/// `vmAlarm*`, and the VM resource-store documents.
struct LegacyPrograms<'a> {
    trx: &'a dyn ITrx,
}

fn record(program: Program) -> ProgramRecord {
    ProgramRecord {
        id: program.id,
        machine_id: program.machine_id,
        runtime: program.runtime,
        path: program.path,
        comment: program.comment,
    }
}

/// The legacy wire shape of a program.
pub(crate) fn program_view(record: ProgramRecord) -> Program {
    Program {
        id: record.id,
        machine_id: record.machine_id,
        runtime: record.runtime,
        path: record.path,
        comment: record.comment,
    }
}

impl LegacyPrograms<'_> {
    fn put_machine_link(&self, machine_id: &str, program_id: &str) {
        if !machine_id.is_empty() {
            self.trx.put_link(
                &format!("machinePrograms::{machine_id}::{program_id}"),
                "true",
            );
        }
    }

    fn delete_machine_link(&self, machine_id: &str, program_id: &str) {
        self.trx.del_key(&format!(
            "link::machinePrograms::{machine_id}::{program_id}"
        ));
    }
}

impl ProgramPorts<'_> {
    /// A program as legacy `Program::pull` returned it: a missing program reads as an
    /// empty record carrying the requested id.
    pub(crate) fn program_or_empty(&self, program_id: &str) -> Program {
        match self.program(program_id).ok().flatten() {
            Some(found) => program_view(found),
            None => Program {
                id: program_id.to_owned(),
                ..Default::default()
            },
        }
    }
}

impl ProgramDirectory for LegacyPrograms<'_> {
    fn program(&self, program_id: &str) -> PortResult<Option<ProgramRecord>> {
        if !self.trx.has_obj(Program::type_(), program_id) {
            return Ok(None);
        }
        Ok(Some(record(
            Program {
                id: program_id.to_owned(),
                ..Default::default()
            }
            .pull(self.trx),
        )))
    }

    fn programs(&self, offset: i64, count: Option<i64>) -> PortResult<Vec<ProgramRecord>> {
        let mut all = self
            .trx
            .get_obj_list(
                Program::type_(),
                &["*".to_owned()],
                &Default::default(),
                &[],
            )
            .map_err(|error| PortError::Failed(error.to_string()))?
            .into_iter()
            .map(|(id, columns)| Program::from_columns(id, &columns))
            .collect::<Vec<_>>();
        all.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(legacy_page(all.into_iter().map(record), offset, count))
    }

    fn programs_of_machine(&self, machine_id: &str) -> PortResult<Vec<ProgramRecord>> {
        let prefix = format!("machinePrograms::{machine_id}::");
        let mut programs = Vec::new();
        for link in self
            .trx
            .get_links_list(&prefix, -1, -1, &[])
            .unwrap_or_default()
        {
            let Some(program_id) = link.strip_prefix(&prefix) else {
                continue;
            };
            if let Some(program) = self.program(program_id)? {
                programs.push(program);
            }
        }
        programs.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(programs)
    }

    fn create_program(&self, record: &ProgramRecord) -> PortResult<()> {
        if self.trx.has_obj(Program::type_(), &record.id) {
            return Err(PortError::Conflict);
        }
        program_view(record.clone()).push(self.trx);
        self.put_machine_link(&record.machine_id, &record.id);
        Ok(())
    }

    fn update_program(&self, record: &ProgramRecord) -> PortResult<()> {
        let current = self.program(&record.id)?.ok_or(PortError::NotFound)?;
        program_view(record.clone()).push(self.trx);
        if current.machine_id != record.machine_id {
            self.delete_machine_link(&current.machine_id, &record.id);
            self.put_machine_link(&record.machine_id, &record.id);
        }
        Ok(())
    }

    fn delete_program(&self, program_id: &str) -> PortResult<()> {
        let Some(current) = self.program(program_id)? else {
            return Ok(());
        };
        for column in PROGRAM_COLUMNS {
            self.trx
                .del_key(&format!("obj::Program::{program_id}::{column}"));
        }
        self.delete_machine_link(&current.machine_id, program_id);
        Ok(())
    }
}

fn program_metadata_key(program_id: &str) -> String {
    format!("ProgMeta::{program_id}")
}

impl ProgramPorts<'_> {
    /// The metadata object at `path`, as legacy `get_json(..).ok()` returned it.
    pub(crate) fn metadata_object(
        &self,
        program_id: &str,
        path: &str,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        let text = self.program_metadata(program_id, path).ok().flatten()?;
        serde_json::from_str(&text).ok()
    }

    /// Deep-merge `document` into the metadata; a non-object is ignored, as legacy
    /// `put_json` failed on it without effect.
    pub(crate) fn merge_metadata_value(
        &self,
        program_id: &str,
        document: &serde_json::Value,
    ) -> PortResult<()> {
        if !document.is_object() {
            return Ok(());
        }
        let text = serde_json::to_string(document)
            .map_err(|error| PortError::Failed(error.to_string()))?;
        self.merge_program_metadata(program_id, &text)
    }
}

impl ProgramMetadata for LegacyPrograms<'_> {
    fn program_metadata(&self, program_id: &str, path: &str) -> PortResult<Option<String>> {
        match self.trx.get_json(&program_metadata_key(program_id), path) {
            Ok(object) => serde_json::to_string(&object)
                .map(Some)
                .map_err(|error| PortError::Failed(error.to_string())),
            Err(_) => Ok(None),
        }
    }

    fn merge_program_metadata(&self, program_id: &str, document: &str) -> PortResult<()> {
        let document = match serde_json::from_str::<serde_json::Value>(document) {
            Ok(object @ serde_json::Value::Object(_)) => object,
            _ => {
                return Err(PortError::Failed(
                    "metadata must be a JSON object".to_owned(),
                ));
            }
        };
        self.trx
            .put_json(
                &program_metadata_key(program_id),
                "metadata",
                &document,
                true,
            )
            .map_err(|error| PortError::Failed(error.to_string()))
    }

    fn delete_program_metadata(&self, program_id: &str) -> PortResult<()> {
        self.trx
            .del_json(&program_metadata_key(program_id), "metadata");
        Ok(())
    }
}

impl ProgramAlarms for LegacyPrograms<'_> {
    fn alarm(&self, program_id: &str) -> PortResult<Option<ProgramAlarm>> {
        let store_id = self.trx.get_link(&format!("vmAlarmStoreId::{program_id}"));
        if store_id.is_empty() {
            return Ok(None);
        }
        let entity = self.trx.get_link(&format!("vmAlarmEntity::{program_id}"));
        Ok(Some(ProgramAlarm {
            store_id,
            // Legacy replays an unreadable time as due immediately.
            fire_at_millis: self
                .trx
                .get_link(&format!("vmAlarmTime::{program_id}"))
                .parse()
                .unwrap_or(0),
            data: self.trx.get_link(&format!("vmAlarmData::{program_id}")),
            entity: if entity.is_empty() {
                DEFAULT_ALARM_ENTITY.to_owned()
            } else {
                entity
            },
        }))
    }

    fn set_alarm(&self, program_id: &str, alarm: &ProgramAlarm) -> PortResult<()> {
        self.trx
            .put_link(&format!("vmAlarmStoreId::{program_id}"), &alarm.store_id);
        self.trx
            .put_link(&format!("vmAlarmData::{program_id}"), &alarm.data);
        self.trx
            .put_link(&format!("vmAlarmEntity::{program_id}"), &alarm.entity);
        self.trx.put_link(
            &format!("vmAlarmTime::{program_id}"),
            &alarm.fire_at_millis.to_string(),
        );
        Ok(())
    }

    fn clear_alarm(&self, program_id: &str) -> PortResult<()> {
        self.trx
            .del_key(&format!("link::vmAlarmStoreId::{program_id}"));
        self.trx
            .del_key(&format!("link::vmAlarmData::{program_id}"));
        self.trx
            .del_key(&format!("link::vmAlarmEntity::{program_id}"));
        self.trx
            .del_key(&format!("link::vmAlarmTime::{program_id}"));
        Ok(())
    }
}

pub(crate) fn resource_store_key(store_id: &str) -> String {
    format!("Json::VmResourceStore::{store_id}")
}

fn failed(error: impl ToString) -> PortError {
    PortError::Failed(error.to_string())
}

impl VmResourceStores for LegacyPrograms<'_> {
    fn resource_store(&self, store_id: &str) -> PortResult<Option<VmResourceStore>> {
        let key = resource_store_key(store_id);
        let Ok(core) = self.trx.get_json(&key, "core") else {
            return Ok(None);
        };
        let text = |name: &str| {
            core.get(name)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let metadata = self.trx.get_json(&key, "metadata").unwrap_or_default();
        Ok(Some(VmResourceStore {
            id: store_id.to_owned(),
            name: text("name"),
            machine_id: text("machineId"),
            metadata: serde_json::to_string(&metadata).map_err(failed)?,
        }))
    }

    fn resource_stores(&self, machine_id: Option<&str>) -> PortResult<Vec<String>> {
        let prefix = match machine_id {
            Some(machine) => format!("vmOwnedStore::{machine}::"),
            None => "vmOwnedStore::".to_owned(),
        };
        let mut stores = self
            .trx
            .get_links_list(&prefix, -1, -1, &[])
            .unwrap_or_default()
            .into_iter()
            .filter_map(|link| {
                let rest = link.strip_prefix(&prefix)?;
                // Without a machine filter the rest is `{machine}::{store}`.
                let store = match machine_id {
                    Some(_) => rest,
                    None => rest.split_once("::")?.1,
                };
                (!store.is_empty()).then(|| store.to_owned())
            })
            .collect::<Vec<_>>();
        stores.sort();
        stores.dedup();
        Ok(stores)
    }

    fn put_resource_store(
        &self,
        store_id: &str,
        name: &str,
        machine_id: &str,
        metadata: &str,
    ) -> PortResult<()> {
        let metadata = match serde_json::from_str::<serde_json::Value>(metadata) {
            Ok(object @ serde_json::Value::Object(_)) => object,
            _ => return Err(failed("metadata must be a JSON object")),
        };
        let current = self.resource_store(store_id)?;
        // LD-21: an update without a machine keeps the owner.
        let machine = if machine_id.is_empty() {
            current
                .as_ref()
                .map(|store| store.machine_id.clone())
                .filter(|machine| !machine.is_empty())
                .ok_or_else(|| failed("a resource store needs a machine"))?
        } else {
            machine_id.to_owned()
        };
        let key = resource_store_key(store_id);
        self.trx
            .put_json(&key, "metadata", &metadata, true)
            .map_err(failed)?;
        let core = serde_json::json!({"id": store_id, "name": name, "machineId": machine});
        self.trx
            .put_json(&key, "core", &core, true)
            .map_err(failed)?;
        if let Some(previous) = current.filter(|store| store.machine_id != machine) {
            self.trx.del_key(&format!(
                "link::vmOwnedStore::{}::{store_id}",
                previous.machine_id
            ));
        }
        self.trx
            .put_link(&format!("vmOwnedStore::{machine}::{store_id}"), "true");
        Ok(())
    }

    fn delete_resource_store(&self, store_id: &str) -> PortResult<()> {
        let Some(current) = self.resource_store(store_id)? else {
            return Ok(());
        };
        // LD-06: the documents are really removed (legacy deleted unprefixed keys).
        let key = resource_store_key(store_id);
        self.trx.del_json(&key, "metadata");
        self.trx.del_json(&key, "core");
        self.trx.del_key(&format!(
            "link::vmOwnedStore::{}::{store_id}",
            current.machine_id
        ));
        Ok(())
    }
}

/// The program ports of one state action, routed per ADR 0026 to the action's
/// PostgreSQL unit of work when the node runs on PostgreSQL, else to legacy.
pub(crate) struct ProgramPorts<'a> {
    pub(crate) trx: &'a dyn ITrx,
}

/// Run `$call` on the adapter for the current provider, bound as `$ports`.
macro_rules! route {
    ($self:ident, |$ports:ident| $call:expr) => {
        match crate::shell::api::model::core_storage::current_unit() {
            Some(unit) => {
                let $ports = aseman_capsule::program::CapsuleProgramPorts { repository: &*unit };
                $call
            }
            None => {
                let $ports = LegacyPrograms { trx: $self.trx };
                $call
            }
        }
    };
}

impl ProgramDirectory for ProgramPorts<'_> {
    fn program(&self, program_id: &str) -> PortResult<Option<ProgramRecord>> {
        route!(self, |ports| ports.program(program_id))
    }
    fn programs(&self, offset: i64, count: Option<i64>) -> PortResult<Vec<ProgramRecord>> {
        route!(self, |ports| ports.programs(offset, count))
    }
    fn programs_of_machine(&self, machine_id: &str) -> PortResult<Vec<ProgramRecord>> {
        route!(self, |ports| ports.programs_of_machine(machine_id))
    }
    fn create_program(&self, record: &ProgramRecord) -> PortResult<()> {
        route!(self, |ports| ports.create_program(record))
    }
    fn update_program(&self, record: &ProgramRecord) -> PortResult<()> {
        route!(self, |ports| ports.update_program(record))
    }
    fn delete_program(&self, program_id: &str) -> PortResult<()> {
        route!(self, |ports| ports.delete_program(program_id))
    }
}

impl ProgramMetadata for ProgramPorts<'_> {
    fn program_metadata(&self, program_id: &str, path: &str) -> PortResult<Option<String>> {
        route!(self, |ports| ports.program_metadata(program_id, path))
    }
    fn merge_program_metadata(&self, program_id: &str, document: &str) -> PortResult<()> {
        route!(self, |ports| ports
            .merge_program_metadata(program_id, document))
    }
    fn delete_program_metadata(&self, program_id: &str) -> PortResult<()> {
        route!(self, |ports| ports.delete_program_metadata(program_id))
    }
}

impl ProgramAlarms for ProgramPorts<'_> {
    fn alarm(&self, program_id: &str) -> PortResult<Option<ProgramAlarm>> {
        route!(self, |ports| ports.alarm(program_id))
    }
    fn set_alarm(&self, program_id: &str, alarm: &ProgramAlarm) -> PortResult<()> {
        route!(self, |ports| ports.set_alarm(program_id, alarm))
    }
    fn clear_alarm(&self, program_id: &str) -> PortResult<()> {
        route!(self, |ports| ports.clear_alarm(program_id))
    }
}

impl VmResourceStores for ProgramPorts<'_> {
    fn resource_store(&self, store_id: &str) -> PortResult<Option<VmResourceStore>> {
        route!(self, |ports| ports.resource_store(store_id))
    }
    fn resource_stores(&self, machine_id: Option<&str>) -> PortResult<Vec<String>> {
        route!(self, |ports| ports.resource_stores(machine_id))
    }
    fn put_resource_store(
        &self,
        store_id: &str,
        name: &str,
        machine_id: &str,
        metadata: &str,
    ) -> PortResult<()> {
        route!(self, |ports| ports
            .put_resource_store(store_id, name, machine_id, metadata))
    }
    fn delete_resource_store(&self, store_id: &str) -> PortResult<()> {
        route!(self, |ports| ports.delete_resource_store(store_id))
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
    fn legacy_programs_pass_the_directory_conformance_suite() {
        let storage: Arc<dyn IStorage> = StubStorage::new();
        let trx = TrxWrapper::new(
            Arc::new(StubCore {
                storage: storage.clone(),
            }),
            storage,
            false,
        );
        let programs = LegacyPrograms { trx: &*trx };
        aseman_ports::conformance::program_directory(&programs, ["1@global", "2@global"]);
        // The derived relation keeps the legacy encoding.
        assert_eq!(
            trx.get_link("machinePrograms::2@global::11@conformance"),
            "true"
        );
        assert_eq!(
            trx.get_link("machinePrograms::1@global::11@conformance"),
            ""
        );
        aseman_ports::conformance::program_metadata(&programs, "12@conformance");
        aseman_ports::conformance::program_alarms(&programs, "12@conformance", "store-1");
        aseman_ports::conformance::vm_resource_stores(&programs, ["1@global", "2@global"]);
    }
}
