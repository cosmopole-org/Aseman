//! The program handlers.

use anyhow::{Result, anyhow};
use aseman_action_sdk::state::creature_ports::{CreaturePorts, creature_view};
use aseman_action_sdk::state::program_ports::{ProgramPorts, program_view};
use aseman_action_sdk::state::store_ports::legacy_error;
use aseman_action_sdk::util::Ctx;
use aseman_action_sdk::wire::program::{
    CreateMachineInput, DeleteProgramInput, ListAppMachsInput, ListInput, UpdateProgramInput,
};
use aseman_application::program::{CreateProgram, DeleteProgram, NewProgram, UpdateProgramPath};
use aseman_ports::{CreatureDirectory, ProgramDirectory, ProgramMetadata};
use serde_json::{Value, json};

pub fn create(ctx: &Ctx<'_>, input: CreateMachineInput) -> Result<Value> {
    let programs = ProgramPorts { trx: ctx.trx };
    let created = CreateProgram {
        creatures: &CreaturePorts { trx: ctx.trx },
        programs: &programs,
    }
    .execute(
        &ctx.caller.user_id,
        NewProgram {
            id: ctx.node.tools().storage().gen_id("global"),
            machine_id: input.app_id,
            runtime: input.runtime,
            path: input.path,
            comment: input.comment,
        },
    )
    .map_err(legacy_error)?;
    programs
        .merge_metadata_value(&created.id, &json!({}))
        .map_err(|error| anyhow!("{error}"))?;
    Ok(json!({"program": program_view(created)}))
}

pub fn update(ctx: &Ctx<'_>, input: UpdateProgramInput) -> Result<Value> {
    let programs = ProgramPorts { trx: ctx.trx };
    let program = UpdateProgramPath {
        creatures: &CreaturePorts { trx: ctx.trx },
        programs: &programs,
    }
    .execute(&ctx.caller.user_id, &input.program_id, &input.path)
    .map_err(legacy_error)?;
    if !input.metadata.is_empty() {
        programs
            .merge_metadata_value(
                &program.id,
                &Value::Object(input.metadata.into_iter().collect()),
            )
            .map_err(|error| anyhow!("{error}"))?;
    }
    Ok(json!({}))
}

/// Delete a program, its relation to its machine, and its metadata (LD-17).
pub fn delete(ctx: &Ctx<'_>, input: DeleteProgramInput) -> Result<Value> {
    let programs = ProgramPorts { trx: ctx.trx };
    DeleteProgram {
        creatures: &CreaturePorts { trx: ctx.trx },
        programs: &programs,
    }
    .execute(&ctx.caller.user_id, &input.program_id)
    .map_err(legacy_error)?;
    programs
        .delete_program_metadata(&input.program_id)
        .map_err(|error| anyhow!("{error}"))?;
    Ok(json!({}))
}

/// A page of every program (`count` -1 is unbounded).
pub fn list(ctx: &Ctx<'_>, input: ListInput) -> Result<Value> {
    let count = (input.count != -1).then_some(input.count);
    let programs = ProgramPorts { trx: ctx.trx }
        .programs(input.offset, count)
        .map_err(|error| anyhow!("{error}"))?
        .into_iter()
        .map(program_view)
        .collect::<Vec<_>>();
    Ok(json!({"machines": programs}))
}

/// The programs of one machine, as the creatures they are, with each one's
/// comment.
pub fn list_program_machines(ctx: &Ctx<'_>, input: ListAppMachsInput) -> Result<Value> {
    let programs = ProgramPorts { trx: ctx.trx }
        .programs_of_machine(&input.app_id)
        .map_err(|error| anyhow!("{error}"))?
        .into_iter()
        .map(program_view);
    let creatures = CreaturePorts { trx: ctx.trx };
    let mut rows = Vec::new();
    for program in programs {
        let Some(record) = creatures
            .creature(&program.id)
            .map_err(|error| anyhow!("{error}"))?
        else {
            continue;
        };
        let creature = creature_view(record, 0);
        rows.push(json!({
            "id": creature.id,
            "type": creature.type_name,
            "username": creature.username,
            "comment": program.comment,
        }));
    }
    Ok(json!({"machines": rows}))
}