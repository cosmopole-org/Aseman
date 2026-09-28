//! The program ports of one state action (ADR 0036): programs, program metadata,
//! alarms, and VM resource stores through the capsule repositories over the action's
//! transaction.

use aseman_domain::creature::legacy_page;
use aseman_domain::program::{DEFAULT_ALARM_ENTITY, ProgramAlarm, ProgramRecord, VmResourceStore};
use aseman_ports::{
    PortError, PortResult, ProgramAlarms, ProgramDirectory, ProgramMetadata, VmResourceStores,
};

use crate::api::model::Program;
use crate::core::trx::Trx;

/// The legacy `Program` object columns, as `Program::push` writes them.
const PROGRAM_COLUMNS: [&str; 6] = ["|", "id", "machineId", "runtime", "path", "comment"];


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



pub(crate) fn resource_store_key(store_id: &str) -> String {
    format!("Json::VmResourceStore::{store_id}")
}

fn failed(error: impl ToString) -> PortError {
    PortError::Failed(error.to_string())
}


/// The program ports of one state action, routed per ADR 0026 to the action's
/// PostgreSQL unit of work when the node runs on PostgreSQL, else to legacy.
pub(crate) struct ProgramPorts<'a> {
    pub(crate) trx: &'a Trx,
}

/// Run `$call` on the adapter for the current provider, bound as `$ports`.
macro_rules! route {
    ($self:ident, |$ports:ident| $call:expr) => {{
        let $ports = aseman_capsule::program::CapsuleProgramPorts { repository: $self.trx };
        $call
    }};
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

    #[test]
    fn programs_pass_the_program_conformance_suites() {
        let trx = crate::core::trx::test_trx();
        let programs = ProgramPorts { trx: &trx };
        aseman_ports::conformance::program_directory(&programs, ["1@global", "2@global"]);
        aseman_ports::conformance::program_metadata(&programs, "12@conformance");
        aseman_ports::conformance::program_alarms(&programs, "12@conformance", "store-1");
        aseman_ports::conformance::vm_resource_stores(&programs, ["1@global", "2@global"]);
    }
}
