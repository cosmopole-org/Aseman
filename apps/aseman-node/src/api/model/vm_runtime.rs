//! VM instance state the node records (ADR 0036): the `core.vm_instance`,
//! `core.vm_distribution`, and `core.vm_terminal` models.
//!
//! A VM instance is launched either as a program entity (`/programs/runEntity`: it
//! has a program and entity) or by the `runVm` host operation (it has an owning
//! program). Its status, start time, billing, and cluster placement are recorded
//! with it.

use anyhow::Result;
use aseman_storage::client::core::{vm_distribution, vm_instance, vm_terminal};
use aseman_storage::{Data, FindMany, Models, Unique, Value, Where};
use serde_json::Map;

use crate::core::trx::{Trx, failed};

pub(crate) use vm_instance::VmInstance;

/// Launch facts of a program-entity instance.
pub(crate) struct Launch<'a> {
    pub(crate) vm_id: &'a str,
    pub(crate) program_id: &'a str,
    pub(crate) entity_id: &'a str,
    pub(crate) started_at_millis: i64,
    pub(crate) distributed: bool,
    pub(crate) billing: Option<Map<String, serde_json::Value>>,
}

fn merge(trx: &Trx, vm_id: &str, data: Data) -> Result<()> {
    let mut create = data.clone();
    create.insert("key".to_owned(), Value::from(vm_id));
    trx.upsert(vm_instance::NAME, &Unique::key(vm_id), create, data)
        .map(drop)
        .map_err(failed)
}

/// Record a running program-entity instance.
pub(crate) fn record_launch(trx: &Trx, launch: &Launch<'_>) -> Result<()> {
    let mut data = Data::from([
        ("program_ref".to_owned(), Value::from(launch.program_id)),
        ("entity_ref".to_owned(), Value::from(launch.entity_id)),
        ("status".to_owned(), Value::from("running")),
        (
            "started_at_millis".to_owned(),
            Value::Int(launch.started_at_millis),
        ),
    ]);
    if launch.distributed {
        data.insert("distributed".to_owned(), Value::Bool(true));
    }
    if let Some(billing) = &launch.billing {
        data.insert(
            "billing".to_owned(),
            Value::Json(serde_json::Value::Object(billing.clone())),
        );
    }
    merge(trx, launch.vm_id, data)
}

/// Record the program that launched `vm_id` through the `runVm` host operation.
pub(crate) fn record_owner(trx: &Trx, vm_id: &str, program_id: &str) -> Result<()> {
    merge(
        trx,
        vm_id,
        Data::from([("owner_program".to_owned(), Value::from(program_id))]),
    )
}

pub(crate) fn instance(trx: &Trx, vm_id: &str) -> Result<Option<VmInstance>> {
    trx.vm_instance()
        .find_unique(vm_instance::by_key(vm_id))
        .map_err(failed)
}

/// The instances of a program entity (every entity with `None`), by VM id.
pub(crate) fn instances_of(
    trx: &Trx,
    program_id: &str,
    entity_id: Option<&str>,
) -> Result<Vec<VmInstance>> {
    let mut filter = vm_instance::program_ref().eq(program_id);
    if let Some(entity_id) = entity_id {
        filter = filter.and(vm_instance::entity_ref().eq(entity_id));
    }
    trx.vm_instance()
        .find_many(FindMany::filter(filter).order_by(vm_instance::key().asc()))
        .map_err(failed)
}

/// Every program-entity instance, by VM id.
pub(crate) fn all_instances(trx: &Trx) -> Result<Vec<VmInstance>> {
    trx.vm_instance()
        .find_many(
            FindMany::filter(vm_instance::program_ref().is_set())
                .order_by(vm_instance::key().asc()),
        )
        .map_err(failed)
}

/// Running instances that carry billing.
pub(crate) fn billed_running(trx: &Trx) -> Result<Vec<VmInstance>> {
    trx.vm_instance()
        .find_many(
            FindMany::filter(
                vm_instance::status()
                    .eq("running")
                    .and(Where::field("billing", aseman_storage::Cond::IsNull(false))),
            )
            .order_by(vm_instance::key().asc()),
        )
        .map_err(failed)
}

/// Replace an instance's billing document.
pub(crate) fn set_billing(
    trx: &Trx,
    vm_id: &str,
    billing: Map<String, serde_json::Value>,
) -> Result<()> {
    merge(
        trx,
        vm_id,
        Data::from([(
            "billing".to_owned(),
            Value::Json(serde_json::Value::Object(billing)),
        )]),
    )
}

/// Stop a program-entity instance: it no longer runs, bills, or lists under its
/// program. A VM some program launched through `runVm` keeps that owner.
pub(crate) fn mark_stopped(trx: &Trx, vm_id: &str) -> Result<()> {
    match instance(trx, vm_id)? {
        Some(row) if row.owner_program.is_some() => trx
            .vm_instance()
            .update(
                vm_instance::by_key(vm_id),
                vm_instance::update()
                    .program_ref(None)
                    .entity_ref(None)
                    .status(None)
                    .started_at_millis(None)
                    .billing(None),
            )
            .map(drop)
            .map_err(failed),
        Some(_) => forget(trx, vm_id),
        None => Ok(()),
    }
}

/// Forget an instance entirely (it was destroyed).
pub(crate) fn forget(trx: &Trx, vm_id: &str) -> Result<()> {
    trx.vm_instance()
        .delete(vm_instance::by_key(vm_id))
        .map(drop)
        .map_err(failed)
}

fn distribution_key(program_id: &str, entity_id: Option<&str>) -> String {
    match entity_id {
        Some(entity_id) => format!("{program_id}::{entity_id}"),
        None => program_id.to_owned(),
    }
}

/// The recorded distribution label of a program (or one of its entities).
pub(crate) fn distribution(trx: &Trx, program_id: &str, entity_id: Option<&str>) -> Result<String> {
    Ok(trx
        .vm_distribution()
        .find_unique(vm_distribution::by_key(distribution_key(program_id, entity_id)))
        .map_err(failed)?
        .map(|row| row.label)
        .unwrap_or_default())
}

pub(crate) fn set_distribution(
    trx: &Trx,
    program_id: &str,
    entity_id: Option<&str>,
    label: &str,
) -> Result<()> {
    let key = distribution_key(program_id, entity_id);
    trx.vm_distribution()
        .upsert(
            vm_distribution::by_key(key.clone()),
            vm_distribution::Create {
                key,
                label: label.to_owned(),
            },
            vm_distribution::update().label(label),
        )
        .map(drop)
        .map_err(failed)
}

fn terminal_key(program_id: &str, vm_id: &str, user_id: &str) -> String {
    format!("{program_id}::{vm_id}::{user_id}")
}

/// Open (or close) a user's terminal on a VM.
pub(crate) fn set_terminal(
    trx: &Trx,
    program_id: &str,
    vm_id: &str,
    user_id: &str,
    open: bool,
) -> Result<()> {
    let key = terminal_key(program_id, vm_id, user_id);
    if open {
        trx.vm_terminal()
            .upsert(
                vm_terminal::by_key(key.clone()),
                vm_terminal::Create {
                    key,
                    program_ref: program_id.to_owned(),
                    vm_ref: vm_id.to_owned(),
                    user_ref: user_id.to_owned(),
                },
                vm_terminal::update(),
            )
            .map(drop)
            .map_err(failed)
    } else {
        trx.vm_terminal()
            .delete(vm_terminal::by_key(key))
            .map(drop)
            .map_err(failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instances_record_list_stop_and_forget() {
        let trx = crate::core::trx::test_trx();
        for (vm, entity) in [("vm-b", "e1"), ("vm-a", "e1"), ("vm-c", "e2")] {
            record_launch(
                &trx,
                &Launch {
                    vm_id: vm,
                    program_id: "p1",
                    entity_id: entity,
                    started_at_millis: 7,
                    distributed: false,
                    billing: (vm == "vm-a").then(Map::new),
                },
            )
            .unwrap();
        }
        record_owner(&trx, "vm-x", "p2").unwrap();
        let ids = |rows: Vec<VmInstance>| rows.into_iter().map(|row| row.key).collect::<Vec<_>>();
        assert_eq!(ids(instances_of(&trx, "p1", Some("e1")).unwrap()), ["vm-a", "vm-b"]);
        assert_eq!(ids(all_instances(&trx).unwrap()).len(), 3);
        assert_eq!(ids(billed_running(&trx).unwrap()), ["vm-a"]);
        mark_stopped(&trx, "vm-a").unwrap();
        assert!(billed_running(&trx).unwrap().is_empty());
        assert!(instance(&trx, "vm-a").unwrap().is_none());
        record_launch(
            &trx,
            &Launch {
                vm_id: "vm-x",
                program_id: "p2",
                entity_id: "e",
                started_at_millis: 1,
                distributed: false,
                billing: None,
            },
        )
        .unwrap();
        mark_stopped(&trx, "vm-x").unwrap();
        assert_eq!(instance(&trx, "vm-x").unwrap().unwrap().owner_program.as_deref(), Some("p2"));
        forget(&trx, "vm-b").unwrap();
        assert!(instance(&trx, "vm-b").unwrap().is_none());
        set_distribution(&trx, "p1", Some("e1"), "cluster").unwrap();
        assert_eq!(distribution(&trx, "p1", Some("e1")).unwrap(), "cluster");
        assert_eq!(distribution(&trx, "p1", None).unwrap(), "");
        set_terminal(&trx, "p1", "vm-a", "u1", true).unwrap();
        set_terminal(&trx, "p1", "vm-a", "u1", false).unwrap();
    }
}
