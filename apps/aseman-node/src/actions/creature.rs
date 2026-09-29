//! Creatures: every being on the network that can act (hold a balance, own
//! resources, hold accesses). Their lifecycle, lookups, types, identity, and
//! direct signals.
//!
//! A creature's base is fixed (id, public key, balance, username); the host
//! registers creature *types* on top of it, each with its own initial balance and
//! custom fields. `human` and `machine` are registered at startup.

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use aseman_application::creature::{
    CreateCreature, CreaturePatch, DeleteCreature, GetCreature, NewCreature, UpdateCreature,
};
use aseman_domain::creature::MetadataKind;
use base64::Engine;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::Ctx;
use super::wire::creature::{
    AuthenticateInput, AuthenticateOutput, CheckSignInput, CreateInput, DeleteInput, FindInput,
    GetByUsernameInput, GetInput, GetOutput, ListInput, MetaInput, SignalInput, UpdateInput,
};
use super::wire::store::Send as StorePacket;
use crate::node::Node;
use crate::state::creature_ports::{CreaturePorts, creature_view};
use crate::state::store_ports::{MembershipPorts, legacy_error};
use crate::state::{Creature, Session, Store};
use crate::storage::Trx;
use crate::util::future::async_once;

/// The node's legacy root identity, the only caller of the operator operations.
const ROOT: &str = crate::actions::authority::LEGACY_ROOT;

const DEFAULT_CREATURE_INITIAL_BALANCE: i64 = 0;
const LEGACY_HUMAN_INITIAL_BALANCE: i64 = 1_000_000_000_000_000;

/// A creature profile field returned with a creature lookup: read from the
/// creature's metadata at `path`, or `default` when unset.
struct ProfileField {
    name: &'static str,
    path: &'static str,
    default: &'static str,
}

const PROFILE_FIELDS: [ProfileField; 4] = [
    ProfileField {
        name: "name",
        path: "metadata.public.profile",
        default: "Anonymous User",
    },
    ProfileField {
        name: "avatar",
        path: "metadata.public.profile",
        default: "avatar",
    },
    ProfileField {
        name: "bio",
        path: "metadata.public.profile",
        default: "I'm a DecillionAI User",
    },
    ProfileField {
        name: "location",
        path: "metadata.public.profile",
        default: "DecillionAI Land",
    },
];

// ── Creature types ────────────────────────────────────────────────────────────

fn creature_type(trx: &Trx, name: &str) -> Option<Map<String, Value>> {
    let spec = aseman_ports::CreatureTypes::creature_type(&CreaturePorts { trx }, name)
        .ok()
        .flatten()?;
    serde_json::from_str(&spec).ok()
}

fn put_creature_type(trx: &Trx, name: &str, spec: &Value) -> Result<()> {
    aseman_ports::CreatureTypes::put_creature_type(
        &CreaturePorts { trx },
        name,
        &serde_json::to_string(spec)?,
    )
    .map_err(|error| anyhow!("{error}"))
}

/// Register the built-in creature types (idempotent), and replace the old
/// built-in human grant without overwriting a host-defined balance.
///
/// # Errors
///
/// Storage failures.
pub(crate) fn install_creature_types(node: &Node) -> Result<()> {
    node.in_action(|trx| {
        for (name, desc) in [
            ("human", "The primary human being on the network."),
            ("machine", "A non-human being that can own programs."),
        ] {
            if creature_type(trx, name).is_none() {
                put_creature_type(
                    trx,
                    name,
                    &json!({
                        "initialBalance": DEFAULT_CREATURE_INITIAL_BALANCE,
                        "customFields": [],
                        "desc": desc,
                    }),
                )?;
            }
        }
        if let Some(mut spec) = creature_type(trx, "human")
            && spec.get("initialBalance").and_then(Value::as_i64)
                == Some(LEGACY_HUMAN_INITIAL_BALANCE)
        {
            spec.insert(
                "initialBalance".to_owned(),
                json!(DEFAULT_CREATURE_INITIAL_BALANCE),
            );
            put_creature_type(trx, "human", &Value::Object(spec))?;
        }
        Ok(())
    })
}

/// A creature type's initial balance. The built-in types have one before their
/// registration runs; any other unregistered type is refused.
fn initial_balance(trx: &Trx, name: &str) -> Result<i64> {
    match creature_type(trx, name) {
        Some(spec) => Ok(spec
            .get("initialBalance")
            .and_then(Value::as_i64)
            .unwrap_or(0)),
        None => match name {
            "human" | "machine" => Ok(DEFAULT_CREATURE_INITIAL_BALANCE),
            other => Err(anyhow!("unknown creature type: {other}")),
        },
    }
}

pub(super) fn types(ctx: &Ctx<'_>, _: ListInput) -> Result<Value> {
    let mut out = Vec::new();
    for (name, spec) in aseman_ports::CreatureTypes::creature_types(&CreaturePorts { trx: ctx.trx })
        .map_err(|error| anyhow!("{error}"))?
    {
        let mut spec: Map<String, Value> = serde_json::from_str(&spec)?;
        spec.insert("name".to_owned(), json!(name));
        out.push(Value::Object(spec));
    }
    Ok(json!({ "types": out }))
}

// ── Lifecycle ─────────────────────────────────────────────────────────────────

/// Create a creature and its first session. Its id is minted in the global id
/// space; its origin is the node the request came from.
pub(super) fn create(ctx: &Ctx<'_>, input: CreateInput) -> Result<Value> {
    let creatures = CreaturePorts { trx: ctx.trx };
    let opening_balance = initial_balance(ctx.trx, &input.typ)?;
    let storage = ctx.node.tools().storage();
    let created = CreateCreature {
        directory: &creatures,
        balances: &creatures,
    }
    .execute(NewCreature {
        id: storage.gen_id("global"),
        creature_type: input.typ.clone(),
        name: input.username.clone(),
        origin: ctx.caller.source.clone(),
        public_key: input.public_key.clone(),
        chain_id: input.chain_id.clone(),
        subchain_id: input.subchain_id.clone(),
        owner_id: input.owner_id.clone(),
        caller_id: ctx.caller.user_id.clone(),
        opening_balance,
    })
    .map_err(legacy_error)?;
    let creature = creature_view(created.record, created.balance);
    let session = Session {
        id: storage.gen_id("global"),
        user_id: creature.id.clone(),
    };
    session.save(ctx.trx)?;
    for kind in [MetadataKind::Creature, MetadataKind::User] {
        creatures
            .replace_metadata_value(kind, &creature.id, &input.metadata)
            .map_err(|error| anyhow!("{error}"))?;
    }
    Ok(json!({"creature": creature, "session": session}))
}

pub(super) fn update(ctx: &Ctx<'_>, input: UpdateInput) -> Result<Value> {
    UpdateCreature {
        directory: &CreaturePorts { trx: ctx.trx },
    }
    .execute(
        &ctx.caller.user_id,
        &input.user_id,
        &ctx.caller.source,
        CreaturePatch {
            public_key: input.public_key,
            creature_type: input.typ,
            name: input.username,
        },
    )
    .map_err(legacy_error)?;
    Ok(json!({}))
}

/// Delete a creature, its metadata, and its store memberships (stores left with
/// no member are deleted, LD-12).
pub(super) fn delete(ctx: &Ctx<'_>, input: DeleteInput) -> Result<Value> {
    let creatures = CreaturePorts { trx: ctx.trx };
    DeleteCreature {
        directory: &creatures,
        balances: &creatures,
    }
    .execute(&ctx.caller.user_id, &input.user_id)
    .map_err(legacy_error)?;
    for kind in [MetadataKind::Creature, MetadataKind::User] {
        aseman_ports::CreatureMetadata::delete_metadata(&creatures, kind, &input.user_id)
            .map_err(|error| anyhow!("{error}"))?;
    }
    MembershipPorts { trx: ctx.trx }
        .remove_member_everywhere(&input.user_id)
        .map_err(|error| anyhow!("{error}"))?;
    Ok(json!({}))
}

/// A creature's metadata document.
pub(super) fn meta(ctx: &Ctx<'_>, input: MetaInput) -> Result<Value> {
    let creatures = CreaturePorts { trx: ctx.trx };
    if aseman_ports::CreatureDirectory::creature(&creatures, &input.user_id)
        .map_err(|error| anyhow!("{error}"))?
        .is_none()
    {
        return Err(anyhow!("user not found"));
    }
    Ok(Value::Object(
        creatures
            .metadata_object(MetadataKind::User, &input.user_id, "metadata")
            .unwrap_or_default(),
    ))
}

// ── Lookups ───────────────────────────────────────────────────────────────────

fn lookups<'a>(creatures: &'a CreaturePorts<'a>) -> GetCreature<'a> {
    GetCreature {
        directory: creatures,
        balances: creatures,
    }
}

pub(super) fn get(ctx: &Ctx<'_>, input: GetInput) -> Result<Value> {
    let creatures = CreaturePorts { trx: ctx.trx };
    let found = lookups(&creatures)
        .by_id(&input.user_id)
        .map_err(legacy_error)?;
    Ok(json!({"creature": creature_view(found.record, found.balance)}))
}

/// A creature and its profile fields.
fn with_profile(ctx: &Ctx<'_>, creature: &Creature) -> Result<Value> {
    let mut user: HashMap<String, Value> = match serde_json::to_value(creature)? {
        Value::Object(fields) => fields.into_iter().collect(),
        _ => HashMap::new(),
    };
    let creatures = CreaturePorts { trx: ctx.trx };
    for field in &PROFILE_FIELDS {
        let value = creatures
            .metadata_object(MetadataKind::User, &creature.id, field.path)
            .and_then(|profile| profile.get(field.name).cloned())
            .unwrap_or_else(|| json!(field.default));
        user.insert(field.name.to_owned(), value);
    }
    Ok(serde_json::to_value(GetOutput { user })?)
}

pub(super) fn get_by_username(ctx: &Ctx<'_>, input: GetByUsernameInput) -> Result<Value> {
    let creatures = CreaturePorts { trx: ctx.trx };
    let found = lookups(&creatures)
        .by_username(&input.username)
        .map_err(legacy_error)?;
    with_profile(ctx, &creature_view(found.record, found.balance))
}

pub(super) fn find(ctx: &Ctx<'_>, input: FindInput) -> Result<Value> {
    let creatures = CreaturePorts { trx: ctx.trx };
    let found = lookups(&creatures)
        .by_username_fragment(&input.username)
        .map_err(legacy_error)?;
    with_profile(ctx, &creature_view(found.record, found.balance))
}

pub(super) fn list(ctx: &Ctx<'_>, input: ListInput) -> Result<Value> {
    let creatures = CreaturePorts { trx: ctx.trx };
    let found = lookups(&creatures)
        .list(
            (!input.param.is_empty()).then_some(input.param.as_str()),
            input.offset,
            Some(input.count),
        )
        .map_err(legacy_error)?
        .into_iter()
        .map(|found| creature_view(found.record, found.balance))
        .collect::<Vec<_>>();
    Ok(json!({"creatures": found}))
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct PageInput {
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    count: i64,
}

/// The machines (creatures of type `machine`) with their profile title, avatar,
/// and description.
pub(super) fn list_machines(ctx: &Ctx<'_>, input: PageInput) -> Result<Value> {
    let creatures = CreaturePorts { trx: ctx.trx };
    let machines = lookups(&creatures)
        .list(Some("machine"), input.offset, Some(input.count))
        .map_err(legacy_error)?
        .into_iter()
        .map(|found| creature_view(found.record, found.balance));
    let mut rows = Vec::new();
    for machine in machines {
        let profile = creatures
            .metadata_object(
                MetadataKind::Creature,
                &machine.id,
                "metadata.public.profile",
            )
            .unwrap_or_default();
        let field = |name: &str, default: &str| {
            profile.get(name).cloned().unwrap_or_else(|| json!(default))
        };
        rows.push(json!({
            "id": machine.id,
            "chainId": machine.chain_id,
            "username": machine.username,
            "ownerId": machine.owner_id,
            "programsCount": machine.machines_count,
            "title": field("title", "untitled"),
            "avatar": field("avatar", ""),
            "desc": field("desc", ""),
        }));
    }
    Ok(json!({"machines": rows}))
}

// ── Identity ──────────────────────────────────────────────────────────────────

/// The authenticated caller's own creature.
pub(super) fn authenticate(ctx: &Ctx<'_>, _: AuthenticateInput) -> Result<Value> {
    let creatures = CreaturePorts { trx: ctx.trx };
    let found = lookups(&creatures)
        .by_id(&ctx.caller.user_id)
        .map_err(legacy_error)?;
    let creature = creature_view(found.record, found.balance);
    let user = HashMap::from([
        ("id".to_owned(), json!(creature.id)),
        ("type".to_owned(), json!(creature.type_name)),
        ("username".to_owned(), json!(creature.username)),
        ("publicKey".to_owned(), json!(creature.public_key)),
        ("balance".to_owned(), json!(creature.balance)),
    ]);
    Ok(serde_json::to_value(AuthenticateOutput {
        authenticated: true,
        user,
    })?)
}

/// Verify a creature's signature over a payload, for the node operator: whether
/// it verifies, and the creature's email when it does.
pub(super) fn check_sign(ctx: &Ctx<'_>, input: CheckSignInput) -> Result<Value> {
    if ctx.caller.user_id != ROOT {
        return Err(anyhow!("access denied"));
    }
    let Ok(data) = base64::engine::general_purpose::STANDARD.decode(&input.payload) else {
        return Ok(json!({"valid": false}));
    };
    let (verified, _, _) =
        ctx.node
            .tools()
            .security()
            .auth_with_signature(&input.user_id, &data, &input.signature);
    if !verified {
        return Ok(json!({"valid": false}));
    }
    let email = aseman_ports::finance_ledger::FinanceLedger::id_to_email(
        &crate::state::finance_ports::FinanceLedgerPorts { trx: ctx.trx },
        &input.user_id,
    )
    .map_err(|error| anyhow!("{error}"))?;
    Ok(json!({"valid": true, "email": email}))
}

// ── Signals ───────────────────────────────────────────────────────────────────

/// A signal from the caller: `all` broadcasts into a store the caller may signal
/// in, `pvp` goes to one creature (or its program). The sender travels without
/// its balance.
pub(super) fn signal(ctx: &Ctx<'_>, input: SignalInput) -> Result<Value> {
    let user_id = ctx.caller.user_id.as_str();
    let mut sender =
        aseman_ports::CreatureDirectory::creature(&CreaturePorts { trx: ctx.trx }, user_id)
            .map_err(|error| anyhow!("{error}"))?
            .map(|record| creature_view(record, 0))
            .unwrap_or_else(|| Creature {
                id: user_id.to_owned(),
                ..Default::default()
            });
    sender.balance = 0;
    let signaler = ctx.node.tools().signaler();
    match input.typ.as_str() {
        "all" => {
            let store_id = if ctx.caller.store_id.is_empty() {
                input.store_id.clone()
            } else {
                ctx.caller.store_id.clone()
            };
            if store_id.is_empty() {
                return Err(anyhow!("storeId is required for broadcast"));
            }
            // Posting into a store is a permission, not mere membership.
            let permissions = aseman_ports::StoreAccess::permissions(
                &MembershipPorts { trx: ctx.trx },
                &store_id,
                user_id,
            )
            .map_err(|error| anyhow!("{error}"))?;
            if !permissions.signal {
                return Err(anyhow!("not allowed to signal in this store"));
            }
            let packet = StorePacket {
                action: "broadcast".to_owned(),
                user: sender,
                data: input.data,
                is_temp: input.temp,
                ..Default::default()
            };
            let except = user_id.to_owned();
            async_once(move || {
                signaler.signal_group(
                    "creatures/signal",
                    &store_id,
                    serde_json::to_value(&packet).unwrap_or(Value::Null),
                    vec![except],
                );
            });
        }
        "pvp" => {
            if input.creature_id.is_empty() {
                return Err(anyhow!("creatureId is required for pvp"));
            }
            let target = if input.program_id.is_empty() {
                input.creature_id.clone()
            } else {
                input.program_id.clone()
            };
            // The store the signal was sent within travels as context, so the
            // target learns which space it came from.
            let packet = StorePacket {
                action: "single".to_owned(),
                user: sender,
                store: Store {
                    id: input.store_id,
                    ..Default::default()
                },
                data: input.data,
                is_temp: input.temp,
                entity_id: input.entity_id,
                correlation_id: input.correlation_id,
                ..Default::default()
            };
            async_once(move || {
                signaler.signal_user(
                    "creatures/signal",
                    &target,
                    serde_json::to_value(&packet).unwrap_or(Value::Null),
                );
            });
        }
        _ => return Err(anyhow!("unknown signal type")),
    }
    Ok(json!({"passed": true}))
}
