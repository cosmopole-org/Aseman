//! Authorization of every registered action (P4-05, ADR 0008, A402/A404).
//!
//! Both entry surfaces run through here before their handlers:
//! - guest host calls ([`authorize_host_call`]), identified by the node-stamped packet
//!   (LD-14, LD-27);
//! - signed shell actions ([`authorize_shell_action`]), identified by their verified
//!   signature.
//!
//! The steps are:
//! 1. The surface maps to its A402 action. An unregistered surface is refused.
//! 2. Facts are resolved server-side about the target, relative to the caller's
//!    principal (the user owning the caller):
//!    - `owner` / `same_creature` when the target belongs to that principal;
//!    - `self` when the target is the caller;
//!    - store permissions from the principal's membership;
//!    - `node_admin` for the node's root identity.
//! 3. The reference provider decides.
//!
//! Actions in [`SHADOW_ACTIONS`] are decided and logged but not refused yet; each
//! names its rollout.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use aseman_domain::authority::{ActionRegistry, Condition, PolicyRequest, ResourceRef};
use aseman_domain::identity::{Subject, SubjectKind};
use aseman_ports::PolicyDecisionPort;
use serde_json::Value as JsonValue;

use crate::models::transaction::ITrx;

/// The legacy root identity (`/creatures/mint`'s hard-coded administrator).
pub(crate) const LEGACY_ROOT: &str = "1@global";

/// Decided and logged, not yet refused, with the rollout that ends the exception:
/// - `network.egress`: until A406 egress grants are issued to existing workloads;
/// - `identity.session.login_by_email`: the custodial login, removed by RL-019
///   (ADR 0019, ADR 0004 window);
/// - `creature.list`: legacy bulk listing, until P7-01 defines discovery scopes.
pub(crate) const SHADOW_ACTIONS: [&str; 3] = [
    "network.egress",
    "identity.session.login_by_email",
    "creature.list",
];

/// What the resolver reads about the node's state.
pub(crate) trait AuthorityLookups {
    /// The user owning a program or creature; empty when unknown or itself a user.
    fn owner_user(&self, id: &str) -> String;
    /// The program that launched a VM, or empty.
    fn vm_program(&self, vm_id: &str) -> String;
    /// A member's store permissions: (read, signal, manage).
    fn store_permissions(&self, store_id: &str, member: &str) -> (bool, bool, bool);
    /// The machine owning a resource store, or empty when absent.
    fn resource_store_machine(&self, store_id: &str) -> String;
    /// Whether the creature is a human user.
    fn is_human(&self, id: &str) -> bool;
}

/// Who is calling.
pub(crate) struct Caller {
    pub(crate) subject: Option<Subject>,
    /// The identities that are the caller itself (program, creature, user).
    pub(crate) ids: Vec<String>,
    /// The user the caller acts for.
    pub(crate) principal: String,
    /// The caller's own VM, when it is a workload.
    pub(crate) vm_id: String,
    pub(crate) node_admin: bool,
}

struct Authority {
    surfaces: BTreeMap<String, String>,
    registry: ActionRegistry,
    policy: aseman_policy_native::RegistryPolicy,
}

fn authority() -> Option<&'static Authority> {
    static AUTHORITY: OnceLock<Option<Authority>> = OnceLock::new();
    AUTHORITY
        .get_or_init(|| {
            let registry = aseman_contracts::security::action_registry().ok()?;
            Some(Authority {
                surfaces: aseman_contracts::security::surface_actions().ok()?,
                policy: aseman_policy_native::RegistryPolicy::new(registry.clone(), "node-v1"),
                registry,
            })
        })
        .as_ref()
}

fn text<'a>(input: &'a JsonValue, fields: &[&str]) -> &'a str {
    fields
        .iter()
        .find_map(|field| {
            input[*field]
                .as_str()
                .map(str::trim)
                .filter(|v| !v.is_empty())
        })
        .unwrap_or("")
}

/// The facts about the target of `action` (on `resource`) for `caller`.
pub(crate) fn resolve_facts(
    lookups: &dyn AuthorityLookups,
    action: &str,
    resource: &str,
    caller: &Caller,
    input: &JsonValue,
) -> BTreeSet<Condition> {
    let principal = caller.principal.as_str();
    let is_caller = |id: &str| !id.is_empty() && caller.ids.iter().any(|own| own == id);
    let belongs = |id: &str| {
        !id.is_empty()
            && (is_caller(id)
                || id == principal
                || (!principal.is_empty() && lookups.owner_user(id) == principal))
    };
    let owned = |held: bool, own: bool| {
        let mut facts = BTreeSet::new();
        if held {
            facts.insert(Condition::Owner);
            facts.insert(Condition::SameCreature);
        }
        if own {
            facts.insert(Condition::SelfResource);
        }
        facts
    };
    // Only a `*.create` action creates its resource; `createAccess` changes a store.
    let creating = action.ends_with(".create");
    let mut facts = match resource {
        "creature" | "account" if !creating => {
            let target = text(input, &["id", "creatureId", "userId"]);
            if target.is_empty() && resource == "account" {
                // The caller's own account.
                owned(true, true)
            } else {
                owned(belongs(target), is_caller(target))
            }
        }
        "creature" | "store" if creating => owned(true, false),
        "store" => {
            let store = text(input, &["storeId", "id"]);
            let mut facts = BTreeSet::new();
            let members = caller.ids.iter().map(String::as_str).chain([principal]);
            for member in members {
                if store.is_empty() || member.is_empty() {
                    continue;
                }
                let (read, signal, manage) = lookups.store_permissions(store, member);
                if read {
                    facts.insert(Condition::StoreRead);
                }
                if signal {
                    facts.insert(Condition::StoreSignal);
                }
                if manage {
                    facts.extend([
                        Condition::StoreManage,
                        Condition::Owner,
                        Condition::SameCreature,
                    ]);
                }
            }
            facts
        }
        "program" | "entity" => {
            let target = text(input, &["programId", "id", "machineId"]);
            owned(creating || belongs(target), is_caller(target))
        }
        "workload" => {
            let vm = text(input, &["vmId"]);
            if !vm.is_empty() && vm != caller.vm_id {
                owned(belongs(&lookups.vm_program(vm)), false)
            } else {
                let program = text(input, &["programId", "machineId"]);
                owned(program.is_empty() || belongs(program), vm == caller.vm_id)
            }
        }
        "resource_store" | "resource_entity" => {
            let store = text(input, &["storeId", "id"]);
            let machine = lookups.resource_store_machine(store);
            if machine.is_empty() {
                // A store that does not exist yet is created as the caller's own.
                owned(store.is_empty() || action == "resource_store.write", false)
            } else {
                owned(belongs(&machine), false)
            }
        }
        // The caller acting on its own state: its secrets, guest data, logs,
        // grants, tokens, and the finance records whose handlers verify it is the
        // party or meter.
        _ => {
            let mut facts = owned(true, true);
            facts.insert(Condition::Counterparty);
            facts
        }
    };
    if caller.node_admin {
        facts.insert(Condition::NodeAdmin);
    }
    facts
}

/// Force the caller-owned shape of creations a guest could otherwise abuse
/// (LD-14: a guest set any owner and any balance on a creature it created).
fn shape_guest_creation(op: &str, principal: &str, input: &mut JsonValue) -> Result<(), String> {
    if !matches!(op, "createCreature" | "createOwnedCreature") {
        return Ok(());
    }
    let Some(object) = input.as_object_mut() else {
        return Ok(());
    };
    let balance = object
        .get("balance")
        .and_then(JsonValue::as_f64)
        .unwrap_or(0.0);
    if balance != 0.0 {
        return Err("a guest cannot fund a creature it creates".to_owned());
    }
    if principal.is_empty() {
        return Err("the caller has no owner to create creatures for".to_owned());
    }
    let requested = object
        .get("ownerId")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    if !requested.is_empty() && requested != principal {
        return Err("a guest creates creatures only for its own owner".to_owned());
    }
    object.insert(
        "ownerId".to_owned(),
        JsonValue::String(principal.to_owned()),
    );
    Ok(())
}

/// The resource the request names, so grants for one resource match only it. Egress
/// names the destination host.
fn target_id(resource: &str, input: &JsonValue) -> String {
    match resource {
        "network" => url::Url::parse(text(input, &["url"]))
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default(),
        "store" | "resource_store" | "resource_entity" => {
            text(input, &["storeId", "id"]).to_owned()
        }
        "creature" => text(input, &["id", "creatureId", "userId"]).to_owned(),
        "program" | "entity" => text(input, &["programId", "id", "machineId"]).to_owned(),
        "workload" => text(input, &["vmId", "programId"]).to_owned(),
        "secret" => text(input, &["name", "secretName"]).to_owned(),
        _ => String::new(),
    }
}

/// The caller's grant chains for `action` (A403). Grants are PostgreSQL state, so
/// they exist only when the action runs in a PostgreSQL unit of work (ADR 0026); the
/// legacy provider has none.
fn grant_chains(
    subject: Option<&Subject>,
    action: &str,
) -> Vec<Vec<aseman_domain::capability::Grant>> {
    let (Some(subject), Some(unit)) = (
        subject,
        crate::shell::api::model::core_storage::current_unit(),
    ) else {
        return Vec::new();
    };
    let store = aseman_capsule_repositories::capability::CapsuleGrantStore { repository: &*unit };
    aseman_application::capability::load_chains(&store, subject, action).unwrap_or_default()
}

/// Decide `surface` for `caller`.
///
/// # Errors
///
/// The refusal message.
pub(crate) fn decide_surface(
    lookups: &dyn AuthorityLookups,
    surface: &str,
    caller: &Caller,
    input: &JsonValue,
    now_millis: i64,
) -> Result<(), String> {
    let authority = authority().ok_or("the action registry did not load")?;
    let action_id = authority
        .surfaces
        .get(surface)
        .ok_or_else(|| format!("unregistered surface {surface}"))?;
    let action = &authority.registry.actions[action_id];
    let resource = ResourceRef {
        kind: action.resource.clone(),
        id: target_id(&action.resource, input),
    };
    let decision = authority
        .policy
        .decide(&PolicyRequest {
            subject: caller.subject,
            action: action_id.clone(),
            resource: resource.clone(),
            facts: resolve_facts(lookups, action_id, &action.resource, caller, input),
            grants: grant_chains(caller.subject.as_ref(), action_id),
            at_millis: now_millis,
        })
        .map_err(|error| error.to_string())?;
    let shadow = !decision.allowed && SHADOW_ACTIONS.contains(&action_id.as_str());
    crate::shell::audit::record(aseman_domain::authority::AuditRecord {
        actor: caller
            .subject
            .map_or_else(|| "anonymous".to_owned(), |subject| subject.to_string()),
        action: action_id.clone(),
        target: format!("{}:{}", resource.kind, resource.id),
        decision: decision.reason.code().to_owned(),
        occurred_at_millis: now_millis,
        details: serde_json::json!({
            "surface": surface,
            "matched": decision.matched.map(Condition::as_str),
            "grant_chain": decision.grant_chain,
            "registry_version": decision.registry_version,
            "policy_version": decision.policy_version,
            "shadow": shadow,
        })
        .to_string(),
    });
    if decision.allowed {
        return Ok(());
    }
    if shadow {
        eprintln!(
            "[policy] shadow deny {action_id} ({})",
            decision.reason.code()
        );
        return Ok(());
    }
    Err(format!(
        "not authorized: {action_id} ({})",
        decision.reason.code()
    ))
}

fn workload_subject(id: &str) -> Subject {
    Subject {
        kind: SubjectKind::Workload,
        id: aseman_domain::Uuid::from_bytes(
            aseman_contracts::legacy_realtime::deterministic_legacy_capsule_id(
                "Workload",
                id.as_bytes(),
            ),
        ),
    }
}

fn creature_subject(lookups: &dyn AuthorityLookups, id: &str) -> Subject {
    Subject {
        kind: if lookups.is_human(id) {
            SubjectKind::User
        } else {
            SubjectKind::Creature
        },
        id: aseman_domain::Uuid::from_bytes(
            aseman_contracts::legacy_realtime::deterministic_legacy_capsule_id(
                "Creature",
                id.as_bytes(),
            ),
        ),
    }
}

/// A guest host call's caller: the node-stamped VM, program, and creature.
pub(crate) fn host_caller(
    lookups: &dyn AuthorityLookups,
    vm_id: &str,
    program_id: &str,
    creature_id: &str,
) -> Caller {
    Caller {
        subject: Some(workload_subject(if vm_id.is_empty() {
            program_id
        } else {
            vm_id
        })),
        ids: [program_id, creature_id]
            .into_iter()
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect(),
        principal: lookups.owner_user(program_id),
        vm_id: vm_id.to_owned(),
        node_admin: false,
    }
}

/// Authorize a guest host call; on success the input may have been shaped.
///
/// # Errors
///
/// The refusal message.
pub(crate) fn authorize_host_call(
    lookups: &dyn AuthorityLookups,
    op: &str,
    caller: &Caller,
    input: &mut JsonValue,
    now_millis: i64,
) -> Result<(), String> {
    decide_surface(
        lookups,
        &format!("unified-host-call {op}"),
        caller,
        input,
        now_millis,
    )?;
    shape_guest_creation(op, &caller.principal, input)
}

/// Authorize a signed shell action by `user_id` (empty for an anonymous caller).
///
/// # Errors
///
/// The refusal message.
pub(crate) fn authorize_shell_action(
    lookups: &dyn AuthorityLookups,
    path: &str,
    user_id: &str,
    input: &JsonValue,
    now_millis: i64,
) -> Result<(), String> {
    let caller = if user_id.is_empty() {
        Caller {
            subject: None,
            ids: Vec::new(),
            principal: String::new(),
            vm_id: String::new(),
            node_admin: false,
        }
    } else {
        let owner = lookups.owner_user(user_id);
        Caller {
            subject: Some(creature_subject(lookups, user_id)),
            ids: vec![user_id.to_owned()],
            principal: if owner.is_empty() {
                user_id.to_owned()
            } else {
                owner
            },
            vm_id: String::new(),
            node_admin: user_id == LEGACY_ROOT,
        }
    };
    decide_surface(
        lookups,
        &format!("signed-shell-action {path}"),
        &caller,
        input,
        now_millis,
    )
}

/// The lookups over one state transaction.
pub(crate) struct TrxLookups<'a> {
    pub(crate) trx: &'a dyn ITrx,
}

impl AuthorityLookups for TrxLookups<'_> {
    fn owner_user(&self, id: &str) -> String {
        if id.is_empty() {
            return String::new();
        }
        let programs = crate::shell::api::model::program_ports::ProgramPorts { trx: self.trx };
        let owner = match aseman_ports::ProgramDirectory::program(&programs, id)
            .ok()
            .flatten()
        {
            Some(record) => {
                let program = crate::shell::api::model::program_ports::program_view(record);
                crate::shell::api::actions::program::resolve_program_owner_machine(
                    self.trx, &program,
                )
                .owner_id
            }
            None => {
                (crate::shell::api::model::creature_ports::CreaturePorts { trx: self.trx })
                    .creature_or_empty(id)
                    .owner_id
            }
        };
        if owner == aseman_domain::creature::HUMAN_OWNER {
            String::new()
        } else {
            owner
        }
    }

    fn vm_program(&self, vm_id: &str) -> String {
        self.trx
            .get_link(&crate::drivers::vmm::host::functions::vm_ownership::owner_link_key(vm_id))
            .trim()
            .to_owned()
    }

    fn store_permissions(&self, store_id: &str, member: &str) -> (bool, bool, bool) {
        let ports = crate::shell::api::model::store_ports::MembershipPorts { trx: self.trx };
        let permissions =
            aseman_ports::StoreAccess::permissions(&ports, store_id, member).unwrap_or_default();
        (permissions.read, permissions.signal, permissions.manage)
    }

    fn resource_store_machine(&self, store_id: &str) -> String {
        if store_id.is_empty() {
            return String::new();
        }
        let ports = crate::shell::api::model::program_ports::ProgramPorts { trx: self.trx };
        aseman_ports::VmResourceStores::resource_store(&ports, store_id)
            .ok()
            .flatten()
            .map(|store| store.machine_id)
            .unwrap_or_default()
    }

    fn is_human(&self, id: &str) -> bool {
        (crate::shell::api::model::creature_ports::CreaturePorts { trx: self.trx })
            .creature_or_empty(id)
            .type_name
            == aseman_domain::creature::HUMAN_CREATURE_TYPE
    }
}

#[cfg(test)]
mod tests;
