//! Translation of `shell/api/actions/creature/creature.go`.
//!
//! Registers every creature lifecycle action against the actor and translates
//! the Go bodies one-to-one. The only deliberate gap is the production
//! Firebase-Auth path inside `/creatures/login`: the Rust workspace does not
//! wire a Firebase SDK, so this port mirrors the Go DEV-mode fallback only
//! (treat `emailToken` as the raw email, or fall back to `username@dev.local`
//! if blank). Wiring the real Firebase verifier is a follow-up.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use base64::Engine;
use chrono::Utc;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::core::actor::model::base::info::Info as BaseInfo;
use crate::core::actor::model::secured::guard::Guard;
use crate::core::actor::model::state::State as ActorState;
use crate::models::action::ExtendedField;
use crate::models::action::ISecureAction;
use crate::models::core::ICore;
use crate::models::input::IInput;
use crate::models::state::IState;
use crate::models::transaction::object_to_map;
use crate::models::transaction::ITrx;
use crate::shell::api::model::creature_ports::{creature_view, LegacyCreatures};
use crate::shell::api::model::store_ports::legacy_error;
use crate::shell::api::model::{Creature, Session, Store};
use crate::shell::api::packets::creatures::{
    AuthenticateInput, AuthenticateOutput, CheckSignInput, ClosePoolInput, ConsumeLockInput,
    CreateHoldInput, CreateInput as CreatureCreateInput, DebitPoolInput, DeleteInput, FindInput,
    GetByUsernameInput, GetFinancialAccountInput, GetHoldInput, GetInput, GetOutput, ListInput,
    ListPayoutsInput, LockTokenInput, LoginInput, LoginOutput, MetaInput, MintInput, OpenPoolInput,
    PaymentAdjustmentInput, PublishFinanceCatalogInput, PublishFinanceQuoteInput,
    ReconcileFinancialSystemInput, RefreshPoolInput, RegisterFinanceNodeInput,
    RegisterFinanceResourceInput, ReleaseHoldInput, ReleasePoolInput, RequestPayoutInput,
    ReservePoolInput, ResolvePayoutInput, RetireFinanceNodeInput, RetireFinanceResourceInput,
    ReviewFinanceResourceInput, SecretGetInput, SecretGrantInput, SecretListGrantedInput,
    SecretListInput, SecretPutInput, SecretRevokeInput, SettleHoldInput, SettlePoolInput,
    SignalInput as CreatureSignalInput, StartHoldInput, StorageUploadInput, TransferInput,
    UpdateInput,
};
use crate::shell::api::packets::stores::Send as StoresSend;
use crate::shell::utils::crypto::{secure_key_pairs, secure_unique_string};
use crate::shell::utils::future::async_once;
use crate::shell::utils::secret_crypto;
use aseman_application::creature::{
    CreateCreature, CreaturePatch, DeleteCreature, GetCreature, NewCreature, UpdateCreature,
};
use aseman_domain::creature::MetadataKind;

use super::util::build_secure_action;

mod finance;
use finance::{
    finance_debt_amount, finance_withdrawable_amount, set_finance_debt_amount,
    set_finance_withdrawable_amount, write_finance_journal,
};

fn user_guard() -> Guard {
    Guard {
        is_user: true,
        is_in_store: false,
        // Preserve applet authentication for legacy non-financial actions.
        allow_applet_sign: true,
    }
}
fn finance_guard() -> Guard {
    Guard {
        is_user: true,
        is_in_store: false,
        allow_applet_sign: false,
    }
}

fn anon_guard() -> Guard {
    Guard::default()
}

fn as_i64(raw: &Value) -> Option<i64> {
    match raw {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

/// Build the `/creatures/create` action.
// ─────────────────────────── Creature type registry ───────────────────────────
//
// A creature is the general model for every being on the network that can act
// (hold a balance, own resources, hold accesses). Its base is fixed — id,
// publicKey, balance, username — and the host system managing Caspar registers
// *customized* creature types on top of that base, each declaring its own
// behaviour (initial balance) and any custom fields.
// Two types are registered out of the box: `human` (the primary being) and
// `machine` (a non-human being that can own programs). Registration is
// idempotent and runs in the install (bootstrap) phase of the shell API.

const DEFAULT_CREATURE_INITIAL_BALANCE: i64 = 0;
const LEGACY_HUMAN_INITIAL_BALANCE: i64 = 1_000_000_000_000_000;

/// The spec of a registered creature type, if present.
fn get_creature_type(trx: &dyn ITrx, name: &str) -> Option<Map<String, Value>> {
    let spec = aseman_ports::CreatureTypes::creature_type(&LegacyCreatures { trx }, name)
        .ok()
        .flatten()?;
    serde_json::from_str(&spec).ok()
}

/// Store a creature type's spec through the registry port.
fn put_creature_type(trx: &dyn ITrx, name: &str, spec: &Value) -> Result<()> {
    aseman_ports::CreatureTypes::put_creature_type(
        &LegacyCreatures { trx },
        name,
        &serde_json::to_string(spec)?,
    )
    .map_err(|error| anyhow!("{error}"))
}

/// Register a creature type only if it does not already exist (idempotent).
fn register_creature_type_if_absent(trx: &dyn ITrx, name: &str, spec: Value) {
    if get_creature_type(trx, name).is_some() {
        return;
    }
    if spec.is_object() {
        let _ = put_creature_type(trx, name, &spec);
    }
}

/// Replace the old built-in human grant without overwriting a host-defined
/// balance. Nodes that already installed the human type otherwise retain the
/// legacy value forever because built-in type registration is idempotent.
fn migrate_legacy_human_balance(trx: &dyn ITrx) {
    let Some(mut spec) = get_creature_type(trx, "human") else {
        return;
    };
    if spec.get("initialBalance").and_then(Value::as_i64) != Some(LEGACY_HUMAN_INITIAL_BALANCE) {
        return;
    }

    spec.insert(
        "initialBalance".to_string(),
        json!(DEFAULT_CREATURE_INITIAL_BALANCE),
    );
    let _ = put_creature_type(trx, "human", &Value::Object(spec));
}

/// Idempotently register the built-in creature types. Safe to call from every
/// namespace's `install()` — the host can register additional custom types the
/// same way.
pub fn install_creature_types(app: Arc<dyn ICore>) {
    app.modify_state(
        false,
        Box::new(|trx: &dyn ITrx| {
            register_creature_type_if_absent(
                trx,
                "human",
                json!({
                    "initialBalance": DEFAULT_CREATURE_INITIAL_BALANCE,
                    "customFields": [],
                    "desc": "The primary human being on the network."
                }),
            );
            register_creature_type_if_absent(
                trx,
                "machine",
                json!({
                    "initialBalance": DEFAULT_CREATURE_INITIAL_BALANCE,
                    "customFields": [],
                    "desc": "A non-human being that can own programs."
                }),
            );
            migrate_legacy_human_balance(trx);
            Ok(())
        }),
    );
}

/// Resolve a creature type's initial balance from the registry. Falls back to
/// the built-in seed values when the registry has not been seeded yet (the very
/// first creature is created before `install` runs), and rejects unknown types.
fn resolve_initial_balance(trx: &dyn ITrx, creature_type: &str) -> Result<i64> {
    match get_creature_type(trx, creature_type) {
        Some(spec) => Ok(spec
            .get("initialBalance")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)),
        None => match creature_type {
            "human" | "machine" => Ok(DEFAULT_CREATURE_INITIAL_BALANCE),
            other => Err(anyhow!("unknown creature type: {}", other)),
        },
    }
}

fn create(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<CreatureCreateInput, _>(
        app,
        "/creatures/create",
        anon_guard(),
        move |state: Arc<dyn IState>, input: CreatureCreateInput| -> Result<Value> {
            let trx = state.trx();
            let creatures = LegacyCreatures { trx: &*trx };
            // Initial balance comes from the registered creature type.
            let opening_balance = resolve_initial_balance(&*trx, &input.typ)?;
            let created = CreateCreature {
                directory: &creatures,
                balances: &creatures,
            }
            .execute(NewCreature {
                id: app_for_handler
                    .tools()
                    .storage()
                    .gen_id(&*trx, &input.origin()),
                creature_type: input.typ.clone(),
                name: input.username.clone(),
                origin: state.source(),
                public_key: input.public_key.clone(),
                chain_id: input.chain_id.clone(),
                subchain_id: input.subchain_id.clone(),
                owner_id: input.owner_id.clone(),
                caller_id: state.info().user_id(),
                opening_balance,
            })
            .map_err(legacy_error)?;
            // Creature is the single record for every being — identity, balance,
            // and program ownership all live here. No separate User/Machine rows.
            let creature = creature_view(created.record, created.balance);
            let session = Session {
                id: app_for_handler
                    .tools()
                    .storage()
                    .gen_id(&*trx, &input.origin()),
                user_id: creature.id.clone(),
            };
            session.push(&*trx);
            for kind in [MetadataKind::Creature, MetadataKind::User] {
                creatures
                    .replace_metadata_value(kind, &creature.id, &input.metadata)
                    .map_err(|error| anyhow!("{error}"))?;
            }
            Ok(json!({"creature": creature, "session": session}))
        },
    )
}

fn get(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<GetInput, _>(
        app,
        "/creatures/get",
        user_guard(),
        move |state: Arc<dyn IState>, input: GetInput| -> Result<Value> {
            let trx = state.trx();
            let creatures = LegacyCreatures { trx: &*trx };
            let found = GetCreature {
                directory: &creatures,
                balances: &creatures,
            }
            .by_id(&input.user_id)
            .map_err(legacy_error)?;
            Ok(json!({"creature": creature_view(found.record, found.balance)}))
        },
    )
}

fn list(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ListInput, _>(
        app,
        "/creatures/list",
        user_guard(),
        move |state: Arc<dyn IState>, input: ListInput| -> Result<Value> {
            let trx = state.trx();
            let directory = LegacyCreatures { trx: &*trx };
            let creatures = GetCreature {
                directory: &directory,
                balances: &directory,
            }
            .list(None, input.offset, Some(input.count))
            .map_err(legacy_error)?
            .into_iter()
            .map(|found| creature_view(found.record, found.balance))
            .collect::<Vec<_>>();
            Ok(json!({"creatures": creatures}))
        },
    )
}

fn transfer(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<TransferInput, _>(
        app,
        "/creatures/transfer",
        finance_guard(),
        move |state: Arc<dyn IState>, input: TransferInput| -> Result<Value> {
            let trx = state.trx();
            if input.amount <= 0 {
                return Err(anyhow!("amount must be greater than zero"));
            }
            let Some(mut from) =
                (LegacyCreatures { trx: &*trx }).account(&state.info().user_id())?
            else {
                return Err(anyhow!("sender creature not found"));
            };
            if from.balance < input.amount {
                return Err(anyhow!("your balance is not enough"));
            }
            let Some(to_id) = aseman_ports::CreatureDirectory::creature_id_by_username(
                &LegacyCreatures { trx: &*trx },
                &input.to_username,
            )
            .map_err(|error| anyhow!("{error}"))?
            else {
                return Err(anyhow!("target creature not found"));
            };
            if to_id == from.id {
                return Err(anyhow!("cannot transfer to the same wallet"));
            }
            let Some(mut to) = (LegacyCreatures { trx: &*trx }).account(&to_id)? else {
                return Err(anyhow!("target creature not found"));
            };
            let from_id = from.id.clone();
            let to_id = to.id.clone();
            let from_withdrawable = finance_withdrawable_amount(&*trx, &from_id)?;
            if from_withdrawable > from.balance {
                return Err(anyhow!("withdrawable balance exceeds available balance"));
            }
            let nonwithdrawable = from.balance - from_withdrawable;
            let sent_withdrawable = input.amount.saturating_sub(nonwithdrawable);
            from.balance = from
                .balance
                .checked_sub(input.amount)
                .ok_or_else(|| anyhow!("sender balance underflow"))?;
            set_finance_withdrawable_amount(
                &*trx,
                &from_id,
                from_withdrawable
                    .checked_sub(sent_withdrawable)
                    .ok_or_else(|| anyhow!("sender withdrawable underflow"))?,
            )?;

            let debt = finance_debt_amount(&*trx, &to_id)?;
            let debt_repaid = debt.min(input.amount);
            let wallet_credit = input.amount - debt_repaid;
            let sent_nonwithdrawable = input.amount - sent_withdrawable;
            let withdrawable_used_for_debt = debt_repaid.saturating_sub(sent_nonwithdrawable);
            let received_withdrawable = sent_withdrawable
                .checked_sub(withdrawable_used_for_debt)
                .ok_or_else(|| anyhow!("target withdrawable underflow"))?;
            to.balance = to
                .balance
                .checked_add(wallet_credit)
                .ok_or_else(|| anyhow!("target balance overflow"))?;
            let to_withdrawable = finance_withdrawable_amount(&*trx, &to_id)?
                .checked_add(received_withdrawable)
                .ok_or_else(|| anyhow!("target withdrawable overflow"))?;
            set_finance_debt_amount(&*trx, &to_id, debt - debt_repaid)?;
            set_finance_withdrawable_amount(&*trx, &to_id, to_withdrawable)?;
            (LegacyCreatures { trx: &*trx }).store_account(&from)?;
            (LegacyCreatures { trx: &*trx }).store_account(&to)?;
            let now = Utc::now().timestamp_millis();
            let journal_id = write_finance_journal(
                &*trx,
                "wallet.transfer",
                "",
                &from_id,
                json!({
                    "entries": [
                        {"account": format!("wallet:{from_id}:available"), "amount": -input.amount},
                        {"account": format!("wallet:{to_id}:available"), "amount": wallet_credit},
                        {"account": format!("wallet:{to_id}:debt"), "amount": -debt_repaid}
                    ],
                    "amount": input.amount,
                    "withdrawableAmount": sent_withdrawable,
                    "debtRepaid": debt_repaid,
                }),
                &[from_id.clone(), to_id.clone()],
                now,
            )?;
            Ok(json!({
                "amount": input.amount,
                "toUserId": to_id,
                "debtRepaid": debt_repaid,
                "journalId": journal_id,
            }))
        },
    )
}

fn signal(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<CreatureSignalInput, _>(
        app,
        "/creatures/signal",
        user_guard(),
        move |state: Arc<dyn IState>, input: CreatureSignalInput| -> Result<Value> {
            let trx = state.trx();
            // A missing sender reads as an empty creature carrying its id, as before.
            let sender_creature = aseman_ports::CreatureDirectory::creature(
                &LegacyCreatures { trx: &*trx },
                &state.info().user_id(),
            )
            .map_err(|error| anyhow!("{error}"))?
            .map(|record| creature_view(record, 0))
            .unwrap_or_else(|| Creature {
                id: state.info().user_id(),
                ..Default::default()
            });
            // The signal carries the sender's Creature identity; balance is
            // zeroed so it is never leaked over the signalling channel.
            let mut sender = sender_creature.clone();
            sender.balance = 0;
            let store_id = state.info().store_id();
            if input.typ == "all" {
                if store_id.is_empty() {
                    return Err(anyhow!("storeId is required for broadcast"));
                }
                // Posting into a store is a permission, not mere membership:
                // a viewer holds `read` without `signal` and is refused here.
                let ports = crate::shell::api::model::store_ports::LegacyMembership { trx: &*trx };
                let permissions = aseman_ports::StoreAccess::permissions(
                    &ports,
                    &store_id,
                    &state.info().user_id(),
                )
                .map_err(|error| anyhow!("{error}"))?;
                if !permissions.signal {
                    return Err(anyhow!("not allowed to signal in this store"));
                }
                let packet = StoresSend {
                    action: "broadcast".to_string(),
                    user: sender.clone(),
                    data: input.data.clone(),
                    is_temp: input.temp,
                    ..Default::default()
                };
                let app_async = app_for_handler.clone();
                let store_id_async = store_id.clone();
                let exception_user_id = state.info().user_id();
                let _ = async_once(move || {
                    app_async.tools().signaler().signal_group(
                        "creatures/signal",
                        &store_id_async,
                        serde_json::to_value(&packet).unwrap_or(Value::Null),
                        true,
                        vec![exception_user_id],
                    );
                });
                return Ok(json!({"passed": true}));
            }
            if input.typ != "pvp" {
                return Err(anyhow!("unknown signal type"));
            }
            if input.creature_id.is_empty() {
                return Err(anyhow!("creatureId is required for pvp"));
            }
            let target_id = if !input.program_id.is_empty() {
                input.program_id.clone()
            } else {
                input.creature_id.clone()
            };
            let packet = StoresSend {
                action: "single".to_string(),
                user: sender,
                // Stamp the store this signal was sent within onto the envelope
                // so the target learns which space (store) it came from — carried
                // as signal context, not buried in the payload. A proxy entity
                // relays this through to its backbone, where an agent scopes
                // in-space tool/sub-agent discovery to it. Sourced from the
                // signal's declared `storeId` (the guard is not store-scoped, so
                // `state.info().store_id()` is not populated here). Empty when the
                // signal is not scoped to a store, in which case `store_is_empty`
                // skips the field entirely.
                store: Store {
                    id: input.store_id.clone(),
                    ..Default::default()
                },
                data: input.data.clone(),
                is_temp: input.temp,
                entity_id: input.entity_id.clone(),
                correlation_id: input.correlation_id.clone(),
                ..Default::default()
            };
            let app_async = app_for_handler.clone();
            let _ = async_once(move || {
                app_async.tools().signaler().signal_user(
                    "creatures/signal",
                    &target_id,
                    serde_json::to_value(&packet).unwrap_or(Value::Null),
                    true,
                );
            });
            Ok(json!({"passed": true}))
        },
    )
}

fn authenticate(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<AuthenticateInput, _>(
        app,
        "/creatures/authenticate",
        user_guard(),
        move |state: Arc<dyn IState>, _: AuthenticateInput| -> Result<Value> {
            let user_id = state.info().user_id();
            // Re-enter /creatures/get on the same trx with the caller as
            // the target user. Mirrors how the Go path stitched together a
            // fresh State+Info pair off mainstate.NewState.
            let inner_info = Arc::new(BaseInfo::new("", ""));
            let inner_state: Arc<dyn IState> =
                Arc::new(ActorState::new(Some(inner_info), Some(state.trx()), ""));
            let get_action = app_for_handler
                .actor()
                .fetch_action("/creatures/get")
                .ok_or_else(|| anyhow!("/creatures/get not registered"))?;
            let typed_input: Arc<dyn IInput> = Arc::new(GetInput {
                user_id: user_id.clone(),
            });
            let (_code, res) = get_action.act(inner_state, typed_input)?;
            let creature: Creature = res
                .get("creature")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let mut user_map: HashMap<String, Value> = HashMap::new();
            user_map.insert("id".to_string(), json!(creature.id));
            user_map.insert("type".to_string(), json!(creature.type_name));
            user_map.insert("username".to_string(), json!(creature.username));
            user_map.insert("publicKey".to_string(), json!(creature.public_key));
            user_map.insert("balance".to_string(), json!(creature.balance));
            Ok(serde_json::to_value(AuthenticateOutput {
                authenticated: true,
                user: user_map,
            })?)
        },
    )
}

fn mint(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<MintInput, _>(
        app,
        "/creatures/mint",
        user_guard(),
        move |state: Arc<dyn IState>, input: MintInput| -> Result<Value> {
            if state.info().user_id() != "1@global" {
                return Err(anyhow!("access denied"));
            }
            if input.amount <= 0 {
                return Err(anyhow!("amount must be greater than zero"));
            }
            let trx = state.trx();

            // Exactly-once, when the caller names the payment it is minting.
            //
            // Minting is the only way tokens come into existence and nothing
            // can claw them back, so a caller interrupted *between* a
            // successful mint and recording that fact has no safe move: retry
            // and the payer is credited twice, don't and they are not credited
            // at all. The marker closes that window — the caller retries with
            // the same key and this handler reports the mint it already
            // applied. Handler writes commit as a single batch (see
            // `TrxWrapper::commit`), so the marker and the balance land
            // together or not at all.
            let marker = match input.idempotency_key.trim() {
                "" => None,
                key => Some(format!("MintApplied::{}", key)),
            };
            if let Some(marker) = &marker {
                let applied = trx.get_link(marker);
                if !applied.is_empty() {
                    return Ok(json!({
                        "applied": false,
                        "alreadyApplied": true,
                        "previous": applied,
                    }));
                }
            }

            // The email→id link resolves the creature directly; credit the
            // single authoritative Creature balance.
            let to_user_id = trx.get_link(&format!("UserEmailToId::{}", input.to_user_email));
            if to_user_id.is_empty() {
                return Err(anyhow!("target user not found"));
            }
            let Some(mut creature) = (LegacyCreatures { trx: &*trx }).account(&to_user_id)? else {
                return Err(anyhow!("target user not found"));
            };
            let debt = finance_debt_amount(&*trx, &creature.id)?;
            let debt_repaid = debt.min(input.amount);
            let wallet_credit = input
                .amount
                .checked_sub(debt_repaid)
                .ok_or_else(|| anyhow!("mint credit underflow"))?;
            creature.balance = creature
                .balance
                .checked_add(wallet_credit)
                .ok_or_else(|| anyhow!("balance overflow"))?;
            (LegacyCreatures { trx: &*trx }).store_account(&creature)?;
            set_finance_debt_amount(&*trx, &creature.id, debt - debt_repaid)?;
            let target_id = creature.id.clone();
            let participants = vec![target_id.clone(), state.info().user_id()];
            let journal_id = write_finance_journal(
                &*trx,
                "payment.credited",
                "",
                &target_id,
                json!({
                    "entries": [
                        {"account": "external:payments", "amount": -input.amount},
                        {"account": format!("wallet:{target_id}:available"), "amount": wallet_credit},
                        {"account": format!("wallet:{target_id}:debt"), "amount": -debt_repaid}
                    ],
                    "grossAmount": input.amount, "walletCredit": wallet_credit,
                    "debtRepaid": debt_repaid, "paymentReference": input.idempotency_key,
                }),
                &participants,
                Utc::now().timestamp_millis(),
            )?;
            if let Some(marker) = &marker {
                trx.put_link(
                    marker,
                    &format!("{}:{}:{}", target_id, input.amount, journal_id),
                );
            }
            Ok(json!({
                "applied": true, "balance": creature.balance,
                "walletCredit": wallet_credit, "debtRepaid": debt_repaid,
                "debt": debt - debt_repaid, "journalId": journal_id,
            }))
        },
    )
}

fn check_sign(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<CheckSignInput, _>(
        app,
        "/creatures/checkSign",
        user_guard(),
        move |state: Arc<dyn IState>, input: CheckSignInput| -> Result<Value> {
            if state.info().user_id() != "1@global" {
                return Err(anyhow!("access denied"));
            }
            let data = match base64::engine::general_purpose::STANDARD.decode(&input.payload) {
                Ok(d) => d,
                Err(e) => {
                    log::warn!("checkSign decode: {}", e);
                    return Ok(json!({"valid": false}));
                }
            };
            let (success, _, _) = app_for_handler.tools().security().auth_with_signature(
                &input.user_id,
                &data,
                &input.signature,
            );
            if success {
                let email = state
                    .trx()
                    .get_link(&format!("UserIdToEmail::{}", input.user_id));
                return Ok(json!({"valid": true, "email": email}));
            }
            Ok(json!({"valid": false}))
        },
    )
}

// ── creature-owned secrets ──────────────────────────────────────────────────
// A creature stores a secret so its value lives on-chain only as ciphertext
// (encrypted under the node master key, off-chain). The owner can always read it
// back; it may grant another creature time-boxed, revocable read access. Access
// control is enforced HERE against the authenticated caller (`user_id()`), which
// a creature cannot forge — the encryption alone is not the boundary.

const SECRET_PREFIX: &str = "Secret::";
const SECRET_GRANT_PREFIX: &str = "SecretGrant::";
const SECRET_GRANTEE_PREFIX: &str = "SecretGrantee::";

fn secret_key(owner: &str, name: &str) -> String {
    format!("{SECRET_PREFIX}{owner}::{name}")
}
fn secret_grant_key(owner: &str, name: &str, grantee: &str) -> String {
    format!("{SECRET_GRANT_PREFIX}{owner}::{name}::{grantee}")
}
/// Reverse index keyed by grantee, so a grantee can enumerate its grants.
fn secret_grantee_key(grantee: &str, owner: &str, name: &str) -> String {
    format!("{SECRET_GRANTEE_PREFIX}{grantee}::{owner}::{name}")
}
/// Names/ids are path components of the storage key, so a ':' would let a caller
/// escape its own namespace. Reject it rather than sanitize silently.
fn valid_component(s: &str) -> bool {
    !s.is_empty() && !s.contains(':')
}

/// The unexpired `{owner, name}` grants held by `grantee`, from the reverse index.
/// Shared by the signed route and the docker host-call so both return the same set.
pub(crate) fn list_granted_secrets(trx: &dyn ITrx, grantee: &str) -> Vec<Value> {
    let prefix = format!("{SECRET_GRANTEE_PREFIX}{grantee}::");
    let now = Utc::now().timestamp_millis();
    let mut out = Vec::new();
    for key in trx.get_by_prefix(&prefix) {
        let Some(rest) = key.strip_prefix(&prefix) else {
            continue;
        };
        // rest = "<owner>::<name>"; owner has no "::" and name has no ':'.
        let Some((owner, name)) = rest.split_once("::") else {
            continue;
        };
        let expires_at: i64 = trx.get_link(&key).trim().parse().unwrap_or(0);
        if expires_at <= 0 || now >= expires_at {
            continue;
        }
        out.push(json!({ "owner": owner, "name": name, "expiresAt": expires_at }));
    }
    out
}

fn secret_put(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_h = app.clone();
    build_secure_action::<SecretPutInput, _>(
        app,
        "/creatures/secretPut",
        user_guard(),
        move |state: Arc<dyn IState>, input: SecretPutInput| -> Result<Value> {
            let owner = state.info().user_id();
            if owner.is_empty() {
                return Err(anyhow!("not authenticated"));
            }
            if !valid_component(&input.name) {
                return Err(anyhow!("secret name is required and must not contain ':'"));
            }
            if input.value.is_empty() {
                return Err(anyhow!("secret value is required"));
            }
            let root = app_h.tools().storage().storage_root().to_string();
            let key = secret_crypto::master_key(&root)?;
            let blob = secret_crypto::encrypt(input.value.as_bytes(), &key)?;
            let trx = state.trx();
            trx.put_link(&secret_key(&owner, &input.name), &blob);
            Ok(json!({ "ok": true, "name": input.name }))
        },
    )
}

fn secret_get(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_h = app.clone();
    build_secure_action::<SecretGetInput, _>(
        app,
        "/creatures/secretGet",
        user_guard(),
        move |state: Arc<dyn IState>, input: SecretGetInput| -> Result<Value> {
            let caller = state.info().user_id();
            if caller.is_empty() {
                return Err(anyhow!("not authenticated"));
            }
            if !valid_component(&input.name) {
                return Err(anyhow!("secret name is required"));
            }
            let owner = if input.owner.is_empty() {
                caller.clone()
            } else {
                input.owner.clone()
            };
            let trx = state.trx();
            // A non-owner caller needs an unexpired grant.
            if owner != caller {
                let raw = trx.get_link(&secret_grant_key(&owner, &input.name, &caller));
                let expires_at: i64 = raw.trim().parse().unwrap_or(0);
                if expires_at <= 0 || Utc::now().timestamp_millis() >= expires_at {
                    return Err(anyhow!("access denied: no valid grant for this secret"));
                }
            }
            let blob = trx.get_link(&secret_key(&owner, &input.name));
            if blob.is_empty() {
                return Err(anyhow!("secret not found"));
            }
            let root = app_h.tools().storage().storage_root().to_string();
            let key = secret_crypto::master_key(&root)?;
            let plaintext = secret_crypto::decrypt(&blob, &key)?;
            let value = String::from_utf8(plaintext)
                .map_err(|_| anyhow!("stored secret is not valid UTF-8"))?;
            Ok(json!({ "ok": true, "owner": owner, "name": input.name, "value": value }))
        },
    )
}

fn secret_grant(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<SecretGrantInput, _>(
        app,
        "/creatures/secretGrant",
        user_guard(),
        move |state: Arc<dyn IState>, input: SecretGrantInput| -> Result<Value> {
            let owner = state.info().user_id();
            if owner.is_empty() {
                return Err(anyhow!("not authenticated"));
            }
            if !valid_component(&input.name) || !valid_component(&input.grantee) {
                return Err(anyhow!(
                    "name and grantee are required and must not contain ':'"
                ));
            }
            if input.ttl_seconds <= 0 {
                return Err(anyhow!("ttlSeconds must be positive"));
            }
            let trx = state.trx();
            // Only the owner of an existing secret may grant access to it.
            if trx.get_link(&secret_key(&owner, &input.name)).is_empty() {
                return Err(anyhow!("secret not found"));
            }
            let expires_at = Utc::now().timestamp_millis() + input.ttl_seconds * 1000;
            trx.put_link(
                &secret_grant_key(&owner, &input.name, &input.grantee),
                &expires_at.to_string(),
            );
            // Reverse index so a grantee can discover what it was granted without
            // knowing the owner up front (secretListGranted). Same expiry value.
            trx.put_link(
                &secret_grantee_key(&input.grantee, &owner, &input.name),
                &expires_at.to_string(),
            );
            Ok(json!({ "ok": true, "grantee": input.grantee, "expiresAt": expires_at }))
        },
    )
}

fn secret_revoke(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<SecretRevokeInput, _>(
        app,
        "/creatures/secretRevoke",
        user_guard(),
        move |state: Arc<dyn IState>, input: SecretRevokeInput| -> Result<Value> {
            let owner = state.info().user_id();
            if owner.is_empty() {
                return Err(anyhow!("not authenticated"));
            }
            if !valid_component(&input.name) || !valid_component(&input.grantee) {
                return Err(anyhow!("name and grantee are required"));
            }
            let trx = state.trx();
            trx.del_key(&secret_grant_key(&owner, &input.name, &input.grantee));
            trx.del_key(&secret_grantee_key(&input.grantee, &owner, &input.name));
            Ok(json!({ "ok": true }))
        },
    )
}

/// List the secrets granted TO the caller (as `{owner, name}` pairs), skipping
/// expired grants. Lets a grantee — e.g. the agent backbone — discover the
/// platform secrets it may read without a hardcoded owner. Names/values are not
/// returned; the caller reads each with `secretGet`.
fn secret_list_granted(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<SecretListGrantedInput, _>(
        app,
        "/creatures/secretListGranted",
        user_guard(),
        move |state: Arc<dyn IState>, _input: SecretListGrantedInput| -> Result<Value> {
            let caller = state.info().user_id();
            if caller.is_empty() {
                return Err(anyhow!("not authenticated"));
            }
            let grants = list_granted_secrets(&*state.trx(), &caller);
            Ok(json!({ "ok": true, "grants": grants }))
        },
    )
}

fn secret_list(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<SecretListInput, _>(
        app,
        "/creatures/secretList",
        user_guard(),
        move |state: Arc<dyn IState>, _input: SecretListInput| -> Result<Value> {
            let owner = state.info().user_id();
            if owner.is_empty() {
                return Err(anyhow!("not authenticated"));
            }
            let prefix = format!("{SECRET_PREFIX}{owner}::");
            let names: Vec<String> = state
                .trx()
                .get_by_prefix(&prefix)
                .into_iter()
                .filter_map(|k| k.strip_prefix(&prefix).map(|s| s.to_string()))
                .collect();
            Ok(json!({ "ok": true, "names": names }))
        },
    )
}

/// Upload a file to the node's public blob storage, authenticated as the caller.
/// The bytes live OFF-chain (under `<storage_root>/public-files`) and only the
/// returned id is meant to go on-chain (e.g. an avatar id in a profile). Served
/// back publicly by the storage HTTP endpoint `GET /storage/file/<id>`. An owner
/// sidecar records who uploaded it, so a file is a user-owned entity.
fn storage_upload(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_h = app.clone();
    build_secure_action::<StorageUploadInput, _>(
        app,
        "/storage/upload",
        user_guard(),
        move |state: Arc<dyn IState>, input: StorageUploadInput| -> Result<Value> {
            let owner = state.info().user_id();
            if owner.is_empty() {
                return Err(anyhow!("not authenticated"));
            }
            let data = base64::engine::general_purpose::STANDARD
                .decode(input.data_base64.trim())
                .map_err(|_| anyhow!("dataBase64 is not valid base64"))?;
            if data.is_empty() {
                return Err(anyhow!("empty file"));
            }
            const MAX: usize = 10 * 1024 * 1024; // same bound as the storage HTTP endpoint
            if data.len() > MAX {
                return Err(anyhow!("file too large (max {MAX} bytes)"));
            }
            let ctype = {
                let c = input.content_type.trim();
                if c.is_empty() {
                    "application/octet-stream".to_string()
                } else {
                    c.to_string()
                }
            };
            let root = format!("{}/public-files", app_h.tools().storage().storage_root());
            let id = uuid::Uuid::new_v4().to_string();
            let file = app_h.tools().file();
            file.save_data_to_global_storage(&root, &data, &id, true)
                .map_err(|e| anyhow!("storage write failed: {e}"))?;
            // Sidecars: content type (so the download round-trips it) + owner.
            let _ = file.save_data_to_global_storage(
                &root,
                ctype.as_bytes(),
                &format!("{id}.type"),
                true,
            );
            let _ = file.save_data_to_global_storage(
                &root,
                owner.as_bytes(),
                &format!("{id}.owner"),
                true,
            );
            Ok(json!({ "ok": true, "id": id, "contentType": ctype }))
        },
    )
}

fn lock_token(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<LockTokenInput, _>(
        app,
        "/creatures/lockToken",
        user_guard(),
        move |state: Arc<dyn IState>, input: LockTokenInput| -> Result<Value> {
            let trx = state.trx();
            // Balance authority is the Creature record (same as transfer/mint).
            let mut user =
                (LegacyCreatures { trx: &*trx }).account_or_empty(&state.info().user_id())?;

            let mut steps: Vec<Value> = Vec::with_capacity(input.steps.len().max(1));
            if !input.steps.is_empty() {
                for (i, step) in input.steps.iter().enumerate() {
                    if step.amount <= 0 {
                        return Err(anyhow!("step {} amount must be greater than zero", i));
                    }
                    if step.unlock_at <= 0 {
                        return Err(anyhow!(
                            "step {} unlockAt must be a unix timestamp in milliseconds",
                            i
                        ));
                    }
                    steps.push(json!({
                        "amount": step.amount,
                        "unlockAt": step.unlock_at,
                        "consumed": false,
                    }));
                }
            } else {
                if input.amount <= 0 {
                    return Err(anyhow!("amount must be greater than zero"));
                }
                if input.unlock_at <= 0 {
                    return Err(anyhow!("unlockAt must be a unix timestamp in milliseconds"));
                }
                steps.push(json!({
                    "amount": input.amount,
                    "unlockAt": input.unlock_at,
                    "consumed": false,
                }));
            }

            let total_amount = steps.iter().try_fold(0_i64, |total, step| {
                let amount = step.get("amount").and_then(|v| v.as_i64()).unwrap_or(0);
                total
                    .checked_add(amount)
                    .ok_or_else(|| anyhow!("lock amount overflow"))
            })?;
            if user.balance < total_amount {
                return Err(anyhow!("your balance is not enough"));
            }
            let lock_id = secure_unique_string();
            if input.typ == "pay" {
                if (LegacyCreatures { trx: &*trx })
                    .account(&input.target)?
                    .is_none()
                {
                    return Err(anyhow!("target user not acceptable"));
                }
                user.balance = user
                    .balance
                    .checked_sub(total_amount)
                    .ok_or_else(|| anyhow!("balance underflow"))?;
                (LegacyCreatures { trx: &*trx }).store_account(&user)?;
                let payload = json!({
                    "type": "pay",
                    "amount": total_amount,
                    "remainingAmount": total_amount,
                    "userId": input.target,
                    "steps": steps,
                });
                trx.put_json(
                    &format!("Json::Creature::{}", state.info().user_id()),
                    &format!("lockedTokens.{}", lock_id),
                    &payload,
                    true,
                )?;
            } else {
                return Err(anyhow!("unknown lock type"));
            }
            Ok(json!({"tokenId": lock_id}))
        },
    )
}

fn consume_lock(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<ConsumeLockInput, _>(
        app,
        "/creatures/consumeLock",
        user_guard(),
        move |state: Arc<dyn IState>, input: ConsumeLockInput| -> Result<Value> {
            let trx = state.trx();
            // Balance authority is the Creature record (same as transfer/mint).
            let mut receiver =
                (LegacyCreatures { trx: &*trx }).account_or_empty(&state.info().user_id())?;
            if input.typ != "pay" {
                return Err(anyhow!("unknown lock type"));
            }
            if (LegacyCreatures { trx: &*trx })
                .account(&input.user_id)?
                .is_none()
            {
                return Err(anyhow!("payer user not found"));
            }
            let sender =
                (LegacyCreatures { trx: &*trx }).account_or_empty(&input.user_id.clone())?;
            let payment_map = match trx.get_json(
                &format!("Json::Creature::{}", sender.id),
                &format!("lockedTokens.{}", input.lock_id),
            ) {
                Ok(m) => m,
                Err(_) => return Err(anyhow!("lock not found")),
            };
            let mut payment: Map<String, Value> = payment_map;
            let steps_raw = match payment.get("steps") {
                Some(Value::Array(arr)) if !arr.is_empty() => arr.clone(),
                _ => return Err(anyhow!("lock does not include steps")),
            };
            let mut step_index: i64 = input.step.unwrap_or(-1);
            let now = Utc::now().timestamp_millis();
            let mut parsed_steps: Vec<Map<String, Value>> = Vec::with_capacity(steps_raw.len());
            let mut parsed_amounts: Vec<i64> = Vec::with_capacity(steps_raw.len());
            let mut parsed_unlocks: Vec<i64> = Vec::with_capacity(steps_raw.len());
            for raw_step in steps_raw.iter() {
                let step_map = match raw_step {
                    Value::Object(o) => o.clone(),
                    _ => return Err(anyhow!("invalid lock step")),
                };
                let step_amount = step_map.get("amount").and_then(as_i64).unwrap_or(0);
                if step_amount <= 0 {
                    return Err(anyhow!("invalid lock step amount"));
                }
                let unlock_at = step_map.get("unlockAt").and_then(as_i64).unwrap_or(0);
                if unlock_at <= 0 {
                    return Err(anyhow!("invalid lock step unlockAt"));
                }
                let consumed = step_map
                    .get("consumed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                parsed_steps.push(step_map);
                parsed_amounts.push(step_amount);
                parsed_unlocks.push(unlock_at);
                if step_index == -1 && !consumed && now >= unlock_at && step_amount == input.amount
                {
                    step_index = (parsed_steps.len() - 1) as i64;
                }
            }
            if step_index < 0 || (step_index as usize) >= parsed_steps.len() {
                return Err(anyhow!("lock step not found"));
            }
            let idx = step_index as usize;
            let selected_step = &mut parsed_steps[idx];
            let selected_amount = parsed_amounts[idx];
            let selected_unlock_at = parsed_unlocks[idx];
            if now < selected_unlock_at {
                return Err(anyhow!("lock step is not consumable yet"));
            }
            if selected_step
                .get("consumed")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                return Err(anyhow!("lock step already consumed"));
            }
            if input.amount != selected_amount {
                return Err(anyhow!("amount of payment not matched"));
            }
            let sign_payload = format!(
                "{}:{}:{}:{}:{}",
                input.lock_id, idx, selected_unlock_at, selected_amount, receiver.id
            );
            let (success, _, _) = app_for_handler.tools().security().auth_with_signature(
                &input.user_id,
                sign_payload.as_bytes(),
                &input.signature,
            );
            if !success {
                return Err(anyhow!("signature not verified"));
            }
            let typ = payment.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if typ != "pay" {
                return Err(anyhow!("type is not payment"));
            }
            let target = payment.get("userId").and_then(|v| v.as_str()).unwrap_or("");
            if target != receiver.id {
                return Err(anyhow!("you are not target"));
            }
            selected_step.insert("consumed".to_string(), Value::Bool(true));
            selected_step.insert("consumedAt".to_string(), json!(now));
            receiver.balance = receiver
                .balance
                .checked_add(input.amount)
                .ok_or_else(|| anyhow!("receiver balance overflow"))?;
            (LegacyCreatures { trx: &*trx }).store_account(&receiver)?;
            let mut remaining_amount: i64 = 0;
            for (i, step_map) in parsed_steps.iter().enumerate() {
                let consumed = step_map
                    .get("consumed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if !consumed {
                    remaining_amount = remaining_amount
                        .checked_add(parsed_amounts[i])
                        .ok_or_else(|| anyhow!("remaining lock amount overflow"))?;
                }
            }
            if remaining_amount == 0 {
                trx.del_json(
                    &format!("Json::Creature::{}", sender.id),
                    &format!("lockedTokens.{}", input.lock_id),
                );
            } else {
                let total_amount = payment.get("amount").and_then(as_i64).unwrap_or(0);
                if total_amount <= 0 {
                    return Err(anyhow!("invalid lock total amount"));
                }
                let steps_value: Value = Value::Array(
                    parsed_steps
                        .iter()
                        .map(|m| Value::Object(m.clone()))
                        .collect(),
                );
                payment.insert("steps".to_string(), steps_value);
                payment.insert("remainingAmount".to_string(), json!(remaining_amount));
                payment.insert(
                    "consumedAmount".to_string(),
                    json!(total_amount - remaining_amount),
                );
                trx.put_json(
                    &format!("Json::Creature::{}", sender.id),
                    &format!("lockedTokens.{}", input.lock_id),
                    &Value::Object(payment),
                    true,
                )?;
            }
            Ok(json!({
                "success": true,
                "step": idx,
                "remainingAmount": remaining_amount,
            }))
        },
    )
}

fn login(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    let app_for_handler = app.clone();
    build_secure_action::<LoginInput, _>(
        app,
        "/creatures/login",
        anon_guard(),
        move |state: Arc<dyn IState>, input: LoginInput| -> Result<Value> {
            // DEV-mode Firebase-Auth fallback. The Go module optionally
            // verified the supplied emailToken with the Firebase Admin SDK;
            // the Rust workspace doesn't carry a Firebase dependency so this
            // port short-circuits straight to the DEV path. Treat the token as
            // the raw email or fall back to a synthetic `username@dev.local`.
            let mut email = input.email_token.trim().to_string();
            let trx = state.trx();
            if crate::drivers::vmm::host::functions::login_grant::grant_mode() {
                // An email alone proves nothing, and for an existing account
                // this path answers with its private key. In grant mode the
                // caller must present a single-use grant a node-owner program
                // issued after verifying the person (password, mail, Google).
                email = email.to_lowercase();
                crate::drivers::vmm::host::functions::login_grant::consume(
                    &*trx,
                    &input.login_grant,
                    &email,
                )?;
            } else {
                if email.is_empty() || !email.contains('@') {
                    email = format!("{}@dev.local", input.username);
                }
                log::info!(
                    "[DEV] firebase disabled; accepting login for email: {}",
                    email
                );
            }

            let user_id = trx.get_link(&format!("UserEmailToId::{}", email));
            if !user_id.is_empty() {
                let creatures = LegacyCreatures { trx: &*trx };
                // LD-13: the old `user.id.is_empty()` check never fired, so a stale
                // email link was never dropped; the directory lookup makes it real.
                let found = aseman_ports::CreatureDirectory::creature(&creatures, &user_id)
                    .map_err(|error| anyhow!("{error}"))?;
                if let Some(record) = found {
                    let user = creature_view(record, creatures.account_or_empty(&user_id)?.balance);
                    let session_id = trx.get_index("Session", "userId", "id", &user.id);
                    let session = Session {
                        id: session_id,
                        ..Default::default()
                    }
                    .pull(&*trx);
                    let private_key = trx.get_link(&format!("UserPrivateKey::{}", user.id));
                    return Ok(serde_json::to_value(LoginOutput {
                        user,
                        session,
                        private_key,
                    })?);
                }
                // Stale email link (creature was deleted but UserEmailToId was
                // not). Drop it so this login mints a new identity.
                trx.del_key(&format!("link::UserEmailToId::{}", email));
            }
            let expected_username = format!("{}@{}", input.username, app_for_handler.id());
            if aseman_ports::CreatureDirectory::creature_id_by_username(
                &LegacyCreatures { trx: &*trx },
                &expected_username,
            )
            .map_err(|error| anyhow!("{error}"))?
            .is_some()
            {
                return Err(anyhow!("username already exist"));
            }
            let (priv_raw, pub_raw) = secure_key_pairs("")?;
            let priv_key = String::from_utf8_lossy(&priv_raw).into_owned();
            let pub_key = String::from_utf8_lossy(&pub_raw).into_owned();
            let create_input = CreatureCreateInput {
                typ: "human".to_string(),
                username: input.username.clone(),
                public_key: pub_key,
                metadata: input.metadata.clone(),
                ..Default::default()
            };
            // Call /creatures/create's action directly on the current state.
            // Going through the secured chain re-submits the request and
            // deadlocks the chain processor (single-threaded), exactly like
            // the Go side.
            let create_action = app_for_handler
                .actor()
                .fetch_action("/creatures/create")
                .ok_or_else(|| anyhow!("/creatures/create not registered"))?;
            let typed_input: Arc<dyn IInput> = Arc::new(create_input);
            let (_code, res) = create_action.act(state.clone(), typed_input)?;
            let creature: Creature = res
                .get("creature")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let session: Session = res
                .get("session")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            trx.put_link(&format!("UserPrivateKey::{}", creature.id), &priv_key);
            trx.put_link(&format!("UserEmailToId::{}", email), &creature.id);
            trx.put_link(&format!("UserIdToEmail::{}", creature.id), &email);
            Ok(serde_json::to_value(LoginOutput {
                user: creature,
                session,
                private_key: priv_key,
            })?)
        },
    )
}

fn delete(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<DeleteInput, _>(
        app,
        "/creatures/delete",
        user_guard(),
        move |state: Arc<dyn IState>, input: DeleteInput| -> Result<Value> {
            let trx = state.trx();
            // LD-13: a missing creature is now "user not found", not a no-op delete.
            let creatures = LegacyCreatures { trx: &*trx };
            DeleteCreature {
                directory: &creatures,
                balances: &creatures,
            }
            .execute(&state.info().user_id(), &input.user_id)
            .map_err(legacy_error)?;
            for kind in [MetadataKind::Creature, MetadataKind::User] {
                aseman_ports::CreatureMetadata::delete_metadata(&creatures, kind, &input.user_id)
                    .map_err(|error| anyhow!("{error}"))?;
            }
            // Memberships go through the store port; the legacy `Store::list(.., -1, -1)`
            // walk here always came back empty (LD-12).
            let ports = crate::shell::api::model::store_ports::LegacyMembership { trx: &*trx };
            ports
                .remove_member_everywhere(&input.user_id)
                .map_err(|error| anyhow!("{error}"))?;
            let email = trx.get_link(&format!("UserIdToEmail::{}", input.user_id));
            trx.del_key(&format!("link::UserIdToEmail::{}", input.user_id));
            if !email.is_empty() {
                trx.del_key(&format!("link::UserEmailToId::{}", email));
            }
            trx.del_key(&format!("link::UserPrivateKey::{}", input.user_id));
            Ok(json!({}))
        },
    )
}

fn update(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<UpdateInput, _>(
        app,
        "/creatures/update",
        user_guard(),
        move |state: Arc<dyn IState>, input: UpdateInput| -> Result<Value> {
            let trx = state.trx();
            // LD-13: a missing creature is now "user not found" instead of being
            // recreated as a partial record.
            UpdateCreature {
                directory: &LegacyCreatures { trx: &*trx },
            }
            .execute(
                &state.info().user_id(),
                &input.user_id,
                &state.source(),
                CreaturePatch {
                    public_key: input.public_key.clone(),
                    creature_type: input.typ.clone(),
                    name: input.username.clone(),
                },
            )
            .map_err(legacy_error)?;
            Ok(json!({}))
        },
    )
}

fn meta(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<MetaInput, _>(
        app,
        "/creatures/meta",
        user_guard(),
        move |state: Arc<dyn IState>, input: MetaInput| -> Result<Value> {
            let trx = state.trx();
            // LD-13: the existence check was dead code; a missing creature now fails.
            let creatures = LegacyCreatures { trx: &*trx };
            if aseman_ports::CreatureDirectory::creature(&creatures, &input.user_id)
                .map_err(|error| anyhow!("{error}"))?
                .is_none()
            {
                return Err(anyhow!("user not found"));
            }
            let m = creatures
                .metadata_object(MetadataKind::User, &input.user_id, "metadata")
                .unwrap_or_default();
            Ok(Value::Object(m))
        },
    )
}

fn apply_extender_fields(
    state: &Arc<dyn IState>,
    trx: &dyn crate::models::transaction::ITrx,
    user_id: &str,
    mut user_map: HashMap<String, Value>,
    extender: &HashMap<String, ExtendedField>,
) -> HashMap<String, Value> {
    // Mirrors the Go `for name, field := range ex { ... }` loop in
    // `creature/creature.go`:
    //   * if the field exposes a `GetValue` callback, invoke it with
    //     `(state, current_map)` and store the result;
    //   * otherwise pull the field from the user's metadata document at
    //     `field.path`, falling back to the declared default.
    // `trx.get_json` always returns the *object* at the given JSON path
    // (`serde_json::Map<String, Value>`), so we extract the specific key
    // from that object to obtain a single `Value`.
    for (key, field) in extender {
        if let Some(get_value) = &field.get_value {
            let snapshot: serde_json::Map<String, Value> = user_map.clone().into_iter().collect();
            if let Ok(v) = get_value(state.clone(), snapshot) {
                user_map.insert(key.clone(), v);
                continue;
            }
        }
        let value = LegacyCreatures { trx }
            .metadata_object(MetadataKind::User, user_id, &field.path)
            .and_then(|m| m.get(key).cloned())
            .unwrap_or_else(|| field.default.clone());
        user_map.insert(key.clone(), value);
    }
    user_map
}

fn get_by_username(
    app: Arc<dyn ICore>,
    user_extender: HashMap<String, ExtendedField>,
) -> Arc<dyn ISecureAction> {
    build_secure_action::<GetByUsernameInput, _>(
        app,
        "/creatures/getByUsername",
        user_guard(),
        move |state: Arc<dyn IState>, input: GetByUsernameInput| -> Result<Value> {
            let trx = state.trx();
            let creatures = LegacyCreatures { trx: &*trx };
            let found = GetCreature {
                directory: &creatures,
                balances: &creatures,
            }
            .by_username(&input.username)
            .map_err(legacy_error)?;
            let result = creature_view(found.record, found.balance);
            let m = object_to_map(&result).unwrap_or_default();
            let user_map: HashMap<String, Value> = m.into_iter().collect();
            let user_map =
                apply_extender_fields(&state, &*trx, &result.id, user_map, &user_extender);
            Ok(serde_json::to_value(GetOutput { user: user_map })?)
        },
    )
}

fn find(
    app: Arc<dyn ICore>,
    user_extender: HashMap<String, ExtendedField>,
) -> Arc<dyn ISecureAction> {
    build_secure_action::<FindInput, _>(
        app,
        "/creatures/find",
        user_guard(),
        move |state: Arc<dyn IState>, input: FindInput| -> Result<Value> {
            let trx = state.trx();
            let creatures = LegacyCreatures { trx: &*trx };
            let found = GetCreature {
                directory: &creatures,
                balances: &creatures,
            }
            .by_username_fragment(&input.username)
            .map_err(legacy_error)?;
            let result = creature_view(found.record, found.balance);
            let m = object_to_map(&result).unwrap_or_default();
            let user_map: HashMap<String, Value> = m.into_iter().collect();
            let user_map =
                apply_extender_fields(&state, &*trx, &result.id, user_map, &user_extender);
            Ok(serde_json::to_value(GetOutput { user: user_map })?)
        },
    )
}

/// List the registered creature types (their specs). Lets the host inspect the
/// extensible type registry.
fn types(app: Arc<dyn ICore>) -> Arc<dyn ISecureAction> {
    build_secure_action::<ListInput, _>(
        app,
        "/creatures/types",
        user_guard(),
        move |state: Arc<dyn IState>, _input: ListInput| -> Result<Value> {
            let trx = state.trx();
            let mut out: Vec<Value> = Vec::new();
            for (name, spec) in
                aseman_ports::CreatureTypes::creature_types(&LegacyCreatures { trx: &*trx })
                    .map_err(|error| anyhow!("{error}"))?
            {
                let mut spec: Map<String, Value> = serde_json::from_str(&spec)?;
                spec.insert("name".to_string(), json!(name));
                out.push(Value::Object(spec));
            }
            Ok(json!({ "types": out }))
        },
    )
}

/// Install every creature action onto the actor.
pub fn install(
    app: Arc<dyn ICore>,
    model_extender: HashMap<String, HashMap<String, ExtendedField>>,
) {
    let user_extender = model_extender.get("user").cloned().unwrap_or_default();
    // Bootstrap phase: register the built-in creature types (idempotent).
    install_creature_types(app.clone());
    let actor = app.actor();
    let mut handlers: Vec<Arc<dyn ISecureAction>> = vec![
        create(app.clone()),
        get(app.clone()),
        list(app.clone()),
        transfer(app.clone()),
        signal(app.clone()),
        authenticate(app.clone()),
        mint(app.clone()),
        check_sign(app.clone()),
        secret_put(app.clone()),
        secret_get(app.clone()),
        secret_grant(app.clone()),
        secret_revoke(app.clone()),
        finance::start_hold_handler(app.clone()),
        secret_list(app.clone()),
        secret_list_granted(app.clone()),
        storage_upload(app.clone()),
    ];
    handlers.extend(finance::handlers(app.clone()));
    handlers.extend([
        lock_token(app.clone()),
        consume_lock(app.clone()),
        login(app.clone()),
        delete(app.clone()),
        update(app.clone()),
        meta(app.clone()),
        get_by_username(app.clone(), user_extender.clone()),
        find(app.clone(), user_extender.clone()),
        types(app.clone()),
    ]);
    for h in handlers {
        actor.inject_secure_action(h);
    }
}
