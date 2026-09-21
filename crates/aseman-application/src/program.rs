//! Program use cases (RL-004 strangler slice: `/programs/create`, `update`, `delete`).
//! Only the owner of a program's machine may change the program (LD-18); legacy
//! checked ownership on create only. Error texts are the legacy ones.

use crate::ApplicationError;
use aseman_domain::program::ProgramRecord;
use aseman_ports::{CreatureDirectory, PortError, ProgramDirectory};

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
