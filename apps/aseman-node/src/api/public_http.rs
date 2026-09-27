//! RL-004 first composition: serve the generated A701 public contract over TLS.
//!
//! This module is the node's half of the public HTTP gateway. The transport
//! (`aseman-public-http`) owns the TLS/HTTP edge and the composed action service
//! (`aseman-public-service`) owns A401 authentication, A402 authorization, execution,
//! and durable idempotency. Here the node supplies the two ports that were missing:
//! [`PublicActionExecutor`] (resolve + execute through migrated application use cases,
//! migrating action families one at a time) and [`LegacySessionDirectory`] (resolve a
//! legacy session token to its subject).
//!
//! The listener starts only when the `ASEMAN_PUBLIC_HTTP_*` configuration is present;
//! an unconfigured node boots exactly as before.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use aseman_application::creature::{
    CreateCreature, CreaturePatch, DeleteCreature, GetCreature, NewCreature, UpdateCreature,
};
use aseman_application::finance as finance_use_cases;
use aseman_application::identity::VerifierPolicy;
use aseman_application::program::{CreateProgram, DeleteProgram, NewProgram, UpdateProgramPath};
use aseman_application::store::{GetStoreAccess, ReadStoreHistory, SetStoreAccess, SignalStore};
use aseman_application::{Diagnostics, GetServerPeers, GetServerPublicKey};
use aseman_capsule::audit::CapsuleDecisionAudit;
use aseman_capsule::capability::CapsuleGrantStore;
use aseman_capsule::identity::CapsuleKeyDirectory;
use aseman_config::{AsemanConfig, FederationListenerConfig, PublicHttpListenerConfig};
use aseman_domain::authority::{ActionRegistry, Condition, ResourceRef};
use aseman_domain::federation::Envelope;
use aseman_domain::identity::Subject;
use aseman_domain::realtime::{can_replay_from, may_deliver};
use aseman_domain::signal_tags::LogQuery;
use aseman_federation_http::{
    FederationExecutor, FederationHttpConfig, FederationResponseSigner, FederationServerTls,
    FederationService, PostgresFederation,
};
use aseman_identity_native::NativeIdentityVerifier;
use aseman_ports::realtime::EventLog;
use aseman_ports::{
    ActionExecutor, BlobStore, DecisionAudit, GrantStore, IdentityVerifier, KeyDirectory,
    PolicyDecisionPort, PortError, PublicActionIdempotency, ReplayGuard, SessionDirectory,
};
use aseman_public_http::{
    PublicActionError, PublicActionRequest, PublicActionResponse, PublicActionService,
    PublicEventBatch, PublicEventFrame, PublicEventRequest, PublicEventService,
    PublicEventSubscription, PublicHttpConfig, PublicTerminalOutput, PublicTerminalRequest,
    PublicTerminalService, PublicTerminalSession,
};
use aseman_public_service::ComposedPublicActionService;
use aseman_realtime_durable::PostgresRealtime;
use aseman_storage_postgres::PostgresCapsuleRepository;
use ring::signature::Ed25519KeyPair;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde_json::{Value, json};

use crate::adapters::blob_store::{PUBLIC_FILES, node_blobs};
use crate::adapters::gateway_subs;
use crate::api::actions::auth::LegacyAuthPorts;
use crate::api::actions::creature as creature_actions;
use crate::api::actions::creature::{
    list_granted_secrets, resolve_initial_balance, secret_grant_key, secret_grantee_key,
    secret_key, valid_component,
};
use crate::api::actions::gateway::{bridge_signal_packet, resolve_bridge_grant};
use crate::api::actions::program as program_actions;
use crate::api::model::creature_ports::{CreaturePorts, creature_view};
use crate::api::model::finance_ports::FinanceLedgerPorts;
use crate::api::model::program_ports::{ProgramPorts, program_view};
use crate::api::model::session::Session;
use crate::api::model::store_ports::{
    MembershipPorts, SignalPorts, StorePorts, legacy_error, log_packet,
};
use crate::api::packets::creatures::{
    CreateInput as CreatureCreateInput, DeleteInput, FindInput, GetInput, ListInput,
    SecretGetInput, SecretGrantInput, SecretListGrantedInput, SecretListInput, SecretPutInput,
    SecretRevokeInput, StorageUploadInput, UpdateInput,
};
use crate::api::packets::gateway::{
    GatewaySignalInput, GatewaySubscribeInput, GatewayUnsubscribeInput,
};
use crate::api::packets::program::{CreateMachineInput, DeleteProgramInput, UpdateProgramInput};
use crate::api::packets::stores::{
    GetAccessInput, HistoryInput, SetAccessInput, SignalInput as StoreSignalInput,
};
use crate::api::utils::future::async_once;
use crate::api::utils::secret_crypto;
use crate::api::workloads::{SystemClock, creature_subject};
use crate::models::core::ICore;
use crate::models::transaction::ITrx;
use base64::Engine;
use chrono::Utc;

/// Resolve and execute a public action.
///
/// `resolve` derives the resource kind from the A402 registry and the resource id
/// from the request body (best effort), establishing `authenticated` facts plus
/// `self` when the subject is the resource. `execute` runs the migrated application
/// use cases; action families are migrated one at a time and anything not yet
/// migrated fails closed.
struct PublicActionExecutor {
    app: Arc<dyn ICore>,
    registry: ActionRegistry,
    clock: SystemClock,
    advertised_port: String,
    origin: String,
    consensus: Option<Arc<dyn aseman_ports::consensus::ConsensusProvider>>,
}

fn resource_id(kind: &str, body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let key: &[&str] = match kind {
        "creature" => &["id", "creatureId", "username", "userId", "name"],
        "store" => &["storeId", "id", "name"],
        "program" => &["programId", "id", "name"],
        _ => &["id", "name", "creatureId", "storeId", "programId", "userId"],
    };
    for candidate in key {
        if let Some(value) = value
            .get(candidate)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_owned());
        }
    }
    None
}

impl PublicActionExecutor {
    /// After a finance action returns `{ "journalId": ... }`, offer the journal record
    /// for ordering through the composed consensus provider (RL-011). Best-effort: the
    /// journal is already durable; consensus ordering failing must not fail the action.
    fn submit_finance_journal(&self, output: &Value) {
        let Some(consensus) = &self.consensus else {
            return;
        };
        let Some(journal_id) = output
            .get("journalId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            return;
        };
        let submit = aseman_application::consensus::SubmitFinanceRecord {
            consensus: &**consensus,
        };
        if let Err(error) = submit.execute(journal_id) {
            log::warn!("finance journal {journal_id} could not be offered for ordering: {error}");
        }
    }

    /// Run a finance action inside one node transaction, then offer any journal it
    /// wrote for ordering through the composed consensus provider (RL-011).
    fn in_finance(
        &self,
        readonly: bool,
        f: impl FnMut(aseman_application::finance::FinancePorts<'_>) -> anyhow::Result<Value>
        + Send
        + 'static,
    ) -> Result<Value, PortError> {
        let output = self.in_finance_trx(readonly, f)?;
        self.submit_finance_journal(&output);
        Ok(output)
    }

    /// Run `f` inside one node state transaction with the finance ports bound, and
    /// return its value.
    fn in_finance_trx<R>(
        &self,
        readonly: bool,
        mut f: impl FnMut(aseman_application::finance::FinancePorts<'_>) -> anyhow::Result<R>
        + Send
        + 'static,
    ) -> Result<R, PortError>
    where
        R: Send + 'static,
    {
        self.in_trx(readonly, move |trx| {
            let ledger = FinanceLedgerPorts { trx };
            let creatures = CreaturePorts { trx };
            let stores = StorePorts { trx };
            let programs = ProgramPorts { trx };
            let membership = MembershipPorts { trx };
            let ports = aseman_application::finance::FinancePorts {
                ledger: &ledger,
                creatures: &creatures,
                balances: &creatures,
                stores: &stores,
                store_metadata: &stores,
                programs: &programs,
                access: &membership,
                clock: &SystemClock,
            };
            f(ports)
        })
    }

    /// Run `f` inside one node state transaction and return its value.
    fn in_trx<R>(
        &self,
        readonly: bool,
        mut f: impl FnMut(&dyn ITrx) -> anyhow::Result<R> + Send + 'static,
    ) -> Result<R, PortError>
    where
        R: Send + 'static,
    {
        let slot = Arc::new(std::sync::Mutex::new(None));
        let holder = slot.clone();
        self.app.modify_state(
            readonly,
            Box::new(move |trx: &dyn ITrx| {
                let value = f(trx).map_err(|error| anyhow!("{error}"))?;
                *holder.lock().unwrap() = Some(value);
                Ok(())
            }),
        );
        slot.lock()
            .unwrap()
            .take()
            .ok_or(PortError::Unavailable("transaction returned no result"))
    }
}

impl ActionExecutor for PublicActionExecutor {
    fn resolve(
        &self,
        subject: &Subject,
        action: &str,
        body: &[u8],
    ) -> Result<(ResourceRef, BTreeSet<Condition>), PortError> {
        let registered = self
            .registry
            .actions
            .get(action)
            .ok_or(PortError::NotFound)?;
        let id = resource_id(&registered.resource, body).unwrap_or_else(|| subject.id.to_string());
        let mut facts = BTreeSet::from([Condition::Authenticated]);
        if registered.rule.contains(&Condition::Public) {
            facts.insert(Condition::Public);
        }
        if id == subject.id.to_string() {
            facts.insert(Condition::SelfResource);
        }
        Ok((
            ResourceRef {
                kind: registered.resource.clone(),
                id,
            },
            facts,
        ))
    }

    fn execute(&self, subject: Subject, action: &str, body: &[u8]) -> Result<Vec<u8>, PortError> {
        let output = match action {
            "node.diagnostics.read" => {
                let input: Value = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("malformed diagnostics input"))?;
                let name = input.get("name").and_then(Value::as_str).unwrap_or("");
                let diagnostics = Diagnostics {
                    clock: &self.clock,
                    advertised_port: &self.advertised_port,
                };
                json!({
                    "hello": diagnostics.hello(name),
                    "time": diagnostics.time_millis(),
                    "port": diagnostics.ping(),
                })
            }
            "node.identity.read" => {
                let ports = LegacyAuthPorts {
                    app: self.app.clone(),
                };
                let public_key = GetServerPublicKey { identity: &ports }
                    .execute()
                    .map_err(|_error| PortError::Unavailable("identity read failed"))?;
                json!({ "publicKey": public_key })
            }
            "node.peers.read" => {
                let ports = LegacyAuthPorts {
                    app: self.app.clone(),
                };
                let peers = GetServerPeers { peers: &ports }
                    .execute()
                    .map_err(|_error| PortError::Unavailable("peers read failed"))?;
                json!({ "peers": peers })
            }
            "creature.create" => {
                let input: CreatureCreateInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad creature.create input"))?;
                let app = self.app.clone();
                let origin = self.origin.clone();
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    let creatures = CreaturePorts { trx };
                    let opening_balance = resolve_initial_balance(trx, &input.typ)?;
                    let id = app.tools().storage().gen_id(trx, "global");
                    let created = CreateCreature {
                        directory: &creatures,
                        balances: &creatures,
                    }
                    .execute(NewCreature {
                        id,
                        creature_type: input.typ.clone(),
                        name: input.username.clone(),
                        origin: origin.clone(),
                        public_key: input.public_key.clone(),
                        chain_id: input.chain_id.clone(),
                        subchain_id: input.subchain_id.clone(),
                        owner_id: input.owner_id.clone(),
                        caller_id: caller.clone(),
                        opening_balance,
                    })
                    .map_err(legacy_error)?;
                    let creature = creature_view(created.record, created.balance);
                    let session = Session {
                        id: app.tools().storage().gen_id(trx, "global"),
                        user_id: creature.id.clone(),
                    };
                    session.push(trx);
                    if input.metadata.is_object() {
                        for kind in [
                            aseman_domain::creature::MetadataKind::Creature,
                            aseman_domain::creature::MetadataKind::User,
                        ] {
                            creatures
                                .replace_metadata_value(kind, &creature.id, &input.metadata)
                                .map_err(|error| anyhow!("{error}"))?;
                        }
                    }
                    Ok(json!({ "creature": creature, "session": session }))
                })?
            }
            "creature.read" => {
                let input: GetInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad creature.read input"))?;
                self.in_trx(true, move |trx| {
                    let creatures = CreaturePorts { trx };
                    let found = GetCreature {
                        directory: &creatures,
                        balances: &creatures,
                    }
                    .by_id(&input.user_id)
                    .map_err(legacy_error)?;
                    Ok(json!({ "creature": creature_view(found.record, found.balance) }))
                })?
            }
            "creature.discover" => {
                let input: FindInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad creature.discover input"))?;
                self.in_trx(true, move |trx| {
                    let creatures = CreaturePorts { trx };
                    let found = GetCreature {
                        directory: &creatures,
                        balances: &creatures,
                    }
                    .by_username_fragment(&input.username)
                    .map_err(legacy_error)?;
                    Ok(json!({ "user": creature_view(found.record, found.balance) }))
                })?
            }
            "creature.list" => {
                let input: ListInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad creature.list input"))?;
                self.in_trx(true, move |trx| {
                    let creatures = CreaturePorts { trx };
                    let found = GetCreature {
                        directory: &creatures,
                        balances: &creatures,
                    }
                    .list(
                        (!input.param.is_empty()).then_some(input.param.as_str()),
                        input.offset,
                        Some(input.count),
                    )
                    .map_err(legacy_error)?;
                    let creatures = found
                        .into_iter()
                        .map(|view| creature_view(view.record, view.balance))
                        .collect::<Vec<_>>();
                    Ok(json!({ "creatures": creatures }))
                })?
            }
            "creature.update" => {
                let input: UpdateInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad creature.update input"))?;
                let origin = self.origin.clone();
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    UpdateCreature {
                        directory: &CreaturePorts { trx },
                    }
                    .execute(
                        &caller,
                        &input.user_id,
                        &origin,
                        CreaturePatch {
                            public_key: input.public_key.clone(),
                            creature_type: input.typ.clone(),
                            name: input.username.clone(),
                        },
                    )
                    .map_err(legacy_error)?;
                    Ok(json!({}))
                })?
            }
            "creature.delete" => {
                let input: DeleteInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad creature.delete input"))?;
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    let creatures = CreaturePorts { trx };
                    DeleteCreature {
                        directory: &creatures,
                        balances: &creatures,
                    }
                    .execute(&caller, &input.user_id)
                    .map_err(legacy_error)?;
                    Ok(json!({}))
                })?
            }
            "store.signal" => {
                let input: StoreSignalInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad store.signal input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    let membership = MembershipPorts { trx };
                    let stores = StorePorts { trx };
                    let signal_log = SignalPorts {
                        storage: app.tools().storage(),
                    };
                    let outcome = SignalStore {
                        stores: &stores,
                        access: &membership,
                        log: &signal_log,
                        clock: &SystemClock,
                    }
                    .execute(
                        &caller,
                        &input.store_id,
                        &input.data,
                        &input.tags,
                        input.temp,
                    )
                    .map_err(legacy_error)?;
                    let signal_id = outcome
                        .signal
                        .as_ref()
                        .map(|signal| signal.id.clone())
                        .unwrap_or_default();
                    Ok(json!({
                        "passed": true,
                        "persisted": outcome.persisted,
                        "signalId": signal_id,
                        "time": outcome.time_millis,
                        "tags": outcome.tags,
                    }))
                })?
            }
            "store.history.read" => {
                let input: HistoryInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad store.history.read input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(true, move |trx| {
                    let membership = MembershipPorts { trx };
                    let signal_log = SignalPorts {
                        storage: app.tools().storage(),
                    };
                    let signals = ReadStoreHistory {
                        access: &membership,
                        log: &signal_log,
                    }
                    .execute(
                        &caller,
                        &input.store_id,
                        LogQuery {
                            tags_all: input.tags_all.clone(),
                            tags_any: input.tags_any.clone(),
                            before_time: input.before_time,
                            after_time: input.after_time,
                            count: input.count,
                        },
                    )
                    .map_err(legacy_error)?;
                    Ok(json!({
                        "storeId": input.store_id,
                        "signals": signals.into_iter().map(log_packet).collect::<Vec<_>>(),
                    }))
                })?
            }
            "store.access.write" => {
                let input: SetAccessInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad store.access.write input"))?;
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    let membership = MembershipPorts { trx };
                    let perms = SetStoreAccess {
                        access: &membership,
                    }
                    .execute(
                        &caller,
                        &input.store_id,
                        &input.member_id,
                        &input.permissions,
                    )
                    .map_err(legacy_error)?;
                    Ok(json!({
                        "storeId": input.store_id,
                        "memberId": input.member_id,
                        "permissions": perms,
                    }))
                })?
            }
            "store.access.read" => {
                let input: GetAccessInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad store.access.read input"))?;
                let caller = subject.id.to_string();
                self.in_trx(true, move |trx| {
                    let membership = MembershipPorts { trx };
                    let (member, perms) = GetStoreAccess {
                        access: &membership,
                    }
                    .execute(&caller, &input.store_id, &input.member_id)
                    .map_err(legacy_error)?;
                    Ok(json!({
                        "storeId": input.store_id,
                        "memberId": member,
                        "permissions": perms,
                    }))
                })?
            }
            "program.create" => {
                let input: CreateMachineInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad program.create input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    let creatures = CreaturePorts { trx };
                    let programs = ProgramPorts { trx };
                    let created = CreateProgram {
                        creatures: &creatures,
                        programs: &programs,
                    }
                    .execute(
                        &caller,
                        NewProgram {
                            id: app.tools().storage().gen_id(trx, "global"),
                            machine_id: input.app_id.clone(),
                            runtime: input.runtime.clone(),
                            path: input.path.clone(),
                            comment: input.comment.clone(),
                        },
                    )
                    .map_err(legacy_error)?;
                    Ok(json!({ "program": program_view(created) }))
                })?
            }
            "program.update" => {
                let input: UpdateProgramInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad program.update input"))?;
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    let programs = ProgramPorts { trx };
                    let program = UpdateProgramPath {
                        creatures: &CreaturePorts { trx },
                        programs: &programs,
                    }
                    .execute(&caller, &input.program_id, &input.path)
                    .map_err(legacy_error)?;
                    if !input.metadata.is_empty() {
                        let meta_value = Value::Object(
                            input
                                .metadata
                                .iter()
                                .map(|(k, v)| (k.clone(), v.clone()))
                                .collect(),
                        );
                        programs
                            .merge_metadata_value(&program.id, &meta_value)
                            .map_err(|error| anyhow!("{error}"))?;
                    }
                    Ok(json!({}))
                })?
            }
            "program.delete" => {
                let input: DeleteProgramInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad program.delete input"))?;
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    DeleteProgram {
                        creatures: &CreaturePorts { trx },
                        programs: &ProgramPorts { trx },
                    }
                    .execute(&caller, &input.program_id)
                    .map_err(legacy_error)?;
                    Ok(json!({}))
                })?
            }
            "secret.write" => {
                let input: SecretPutInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad secret.write input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    if caller.is_empty() {
                        return Err(anyhow!("not authenticated"));
                    }
                    if !valid_component(&input.name) {
                        return Err(anyhow!("secret name is required and must not contain ':'"));
                    }
                    if input.value.is_empty() {
                        return Err(anyhow!("secret value is required"));
                    }
                    let root = app.tools().storage().storage_root().to_string();
                    let key = secret_crypto::master_key(&root)?;
                    let blob = secret_crypto::encrypt(input.value.as_bytes(), &key)?;
                    trx.put_link(&secret_key(&caller, &input.name), &blob);
                    Ok(json!({ "ok": true, "name": input.name }))
                })?
            }
            "secret.read" => {
                let input: SecretGetInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad secret.read input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(true, move |trx| {
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
                    let root = app.tools().storage().storage_root().to_string();
                    let key = secret_crypto::master_key(&root)?;
                    let plaintext = secret_crypto::decrypt(&blob, &key)?;
                    let value = String::from_utf8(plaintext)
                        .map_err(|_| anyhow!("stored secret is not valid UTF-8"))?;
                    Ok(json!({ "ok": true, "owner": owner, "name": input.name, "value": value }))
                })?
            }
            "secret.grant" => {
                let input: SecretGrantInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad secret.grant input"))?;
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    if caller.is_empty() {
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
                    if trx.get_link(&secret_key(&caller, &input.name)).is_empty() {
                        return Err(anyhow!("secret not found"));
                    }
                    let expires_at = Utc::now().timestamp_millis() + input.ttl_seconds * 1000;
                    trx.put_link(
                        &secret_grant_key(&caller, &input.name, &input.grantee),
                        &expires_at.to_string(),
                    );
                    trx.put_link(
                        &secret_grantee_key(&input.grantee, &caller, &input.name),
                        &expires_at.to_string(),
                    );
                    Ok(json!({ "ok": true, "grantee": input.grantee, "expiresAt": expires_at }))
                })?
            }
            "secret.revoke" => {
                let input: SecretRevokeInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad secret.revoke input"))?;
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    if caller.is_empty() {
                        return Err(anyhow!("not authenticated"));
                    }
                    if !valid_component(&input.name) || !valid_component(&input.grantee) {
                        return Err(anyhow!("name and grantee are required"));
                    }
                    trx.del_key(&secret_grant_key(&caller, &input.name, &input.grantee));
                    trx.del_key(&secret_grantee_key(&input.grantee, &caller, &input.name));
                    Ok(json!({ "ok": true }))
                })?
            }
            "secret.list_granted" => {
                let _input: SecretListGrantedInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad secret.list_granted input"))?;
                let caller = subject.id.to_string();
                self.in_trx(true, move |trx| {
                    if caller.is_empty() {
                        return Err(anyhow!("not authenticated"));
                    }
                    let grants = list_granted_secrets(trx, &caller);
                    Ok(json!({ "ok": true, "grants": grants }))
                })?
            }
            "secret.list" => {
                let _input: SecretListInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad secret.list input"))?;
                let caller = subject.id.to_string();
                self.in_trx(true, move |trx| {
                    if caller.is_empty() {
                        return Err(anyhow!("not authenticated"));
                    }
                    let prefix = format!("Secret::{caller}::");
                    let names: Vec<String> = trx
                        .get_by_prefix(&prefix)
                        .into_iter()
                        .filter_map(|key| key.strip_prefix(&prefix).map(str::to_owned))
                        .collect();
                    Ok(json!({ "ok": true, "names": names }))
                })?
            }
            "topic.subscribe" => {
                let input: GatewaySubscribeInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad topic.subscribe input"))?;
                self.in_trx(true, move |trx| {
                    let Some(grant) = resolve_bridge_grant(trx, &input.token) else {
                        return Err(anyhow!("invalid or expired bridge token"));
                    };
                    let requested: Vec<String> = input
                        .topics
                        .iter()
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty())
                        .collect();
                    let topics: Vec<String> = if requested.is_empty() {
                        grant.topics.clone()
                    } else {
                        requested
                            .into_iter()
                            .filter(|t| grant.topics.iter().any(|g| g == t))
                            .collect()
                    };
                    if topics.is_empty() {
                        return Err(anyhow!("token grants none of the requested topics"));
                    }
                    Ok(json!({
                        "ok": true,
                        "gatewaySubscribe": {
                            "topics": topics.clone(),
                            "creatureId": grant.creature_id.clone(),
                        },
                        "topics": topics,
                        "creatureId": grant.creature_id,
                        "expiresAt": grant.expires_at,
                    }))
                })?
            }
            "topic.unsubscribe" => {
                let input: GatewayUnsubscribeInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad topic.unsubscribe input"))?;
                self.in_trx(true, move |trx| {
                    if resolve_bridge_grant(trx, &input.token).is_none() {
                        return Err(anyhow!("invalid or expired bridge token"));
                    }
                    Ok(json!({ "ok": true, "gatewayUnsubscribe": true }))
                })?
            }
            "topic.publish" => {
                let input: GatewaySignalInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad topic.publish input"))?;
                let app = self.app.clone();
                self.in_trx(true, move |trx| {
                    let Some(grant) = resolve_bridge_grant(trx, &input.token) else {
                        return Err(anyhow!("invalid or expired bridge token"));
                    };
                    let topic = input.topic.trim().to_string();
                    if !topic.is_empty() && !grant.topics.iter().any(|t| t == &topic) {
                        return Err(anyhow!("token does not grant this topic"));
                    }
                    let action = input.action.trim().to_string();
                    if action.is_empty() {
                        return Err(anyhow!("action is required"));
                    }
                    let packet = bridge_signal_packet(&input, &topic, &grant.creature_id);
                    let creature_id = grant
                        .routes
                        .get(&action)
                        .cloned()
                        .unwrap_or_else(|| grant.deliver_to.clone());
                    let app_async = app.clone();
                    let target = creature_id.clone();
                    let _ = async_once(move || {
                        app_async.tools().signaler().signal_user(
                            "creatures/signal",
                            &target,
                            packet,
                            true,
                        );
                    });
                    Ok(json!({
                        "ok": true,
                        "creatureId": creature_id,
                        "correlationId": input.correlation_id,
                    }))
                })?
            }
            "file.upload" => {
                let input: StorageUploadInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad file.upload input"))?;
                let app = self.app.clone();
                let owner = subject.id.to_string();
                let data = base64::engine::general_purpose::STANDARD
                    .decode(input.data_base64.trim())
                    .map_err(|_error| PortError::Unavailable("dataBase64 is not valid base64"))?;
                if data.is_empty() {
                    return Err(PortError::Unavailable("empty file"));
                }
                const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;
                if data.len() > MAX_FILE_BYTES {
                    return Err(PortError::Unavailable("file too large"));
                }
                let ctype = {
                    let c = input.content_type.trim();
                    if c.is_empty() {
                        "application/octet-stream".to_string()
                    } else {
                        c.to_string()
                    }
                };
                let id = uuid::Uuid::new_v4().to_string();
                let blobs = node_blobs(&*app.tools().storage());
                blobs
                    .put_blob(&[PUBLIC_FILES, "/", &id].concat(), &data, &ctype, true)
                    .map_err(|_error| PortError::Unavailable("storage write failed"))?;
                let _ = blobs.put_blob(
                    &[PUBLIC_FILES, "/", &id, ".type"].concat(),
                    ctype.as_bytes(),
                    "text/plain",
                    true,
                );
                let _ = blobs.put_blob(
                    &[PUBLIC_FILES, "/", &id, ".owner"].concat(),
                    owner.as_bytes(),
                    "text/plain",
                    true,
                );
                json!({ "ok": true, "id": id, "contentType": ctype })
            }
            "finance.catalog.publish" => {
                let input: finance_use_cases::PublishFinanceCatalogInput =
                    serde_json::from_slice(body).map_err(|_error| {
                        PortError::Unavailable("bad finance.catalog.publish input")
                    })?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::publish_finance_catalog(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.node.register" => {
                let input: finance_use_cases::RegisterFinanceNodeInput =
                    serde_json::from_slice(body).map_err(|_error| {
                        PortError::Unavailable("bad finance.node.register input")
                    })?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::register_finance_node(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.node.retire" => {
                let input: finance_use_cases::RetireFinanceNodeInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.node.retire input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::retire_finance_node(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.resource.register" => {
                let input: finance_use_cases::RegisterFinanceResourceInput =
                    serde_json::from_slice(body).map_err(|_error| {
                        PortError::Unavailable("bad finance.resource.register input")
                    })?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::register_finance_resource(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.resource.review" => {
                let input: finance_use_cases::ReviewFinanceResourceInput =
                    serde_json::from_slice(body).map_err(|_error| {
                        PortError::Unavailable("bad finance.resource.review input")
                    })?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::review_finance_resource(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.resource.retire" => {
                let input: finance_use_cases::RetireFinanceResourceInput =
                    serde_json::from_slice(body).map_err(|_error| {
                        PortError::Unavailable("bad finance.resource.retire input")
                    })?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::retire_finance_resource(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.quote.publish" => {
                let input: finance_use_cases::PublishFinanceQuoteInput =
                    serde_json::from_slice(body).map_err(|_error| {
                        PortError::Unavailable("bad finance.quote.publish input")
                    })?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::publish_finance_quote(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.hold.create" => {
                let input: finance_use_cases::CreateHoldInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.hold.create input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::create_hold(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.hold.start" => {
                let input: finance_use_cases::StartHoldInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.hold.start input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::start_hold(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.hold.settle" => {
                let input: finance_use_cases::SettleHoldInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.hold.settle input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::settle_hold(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.hold.release" => {
                let input: finance_use_cases::ReleaseHoldInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.hold.release input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::release_hold(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.hold.read" => {
                let input: finance_use_cases::GetHoldInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.hold.read input"))?;
                let caller = subject.id.to_string();
                self.in_finance(true, move |ports| {
                    finance_use_cases::get_hold(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.account.read" => {
                let input: finance_use_cases::GetFinancialAccountInput =
                    serde_json::from_slice(body).map_err(|_error| {
                        PortError::Unavailable("bad finance.account.read input")
                    })?;
                let caller = subject.id.to_string();
                self.in_finance(true, move |ports| {
                    finance_use_cases::get_financial_account(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.payout.request" => {
                let input: finance_use_cases::RequestPayoutInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.payout.request input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::request_payout(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.payout.resolve" => {
                let input: finance_use_cases::ResolvePayoutInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.payout.resolve input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::resolve_payout(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.payout.list" => {
                let input: finance_use_cases::ListPayoutsInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.payout.list input"))?;
                let caller = subject.id.to_string();
                self.in_finance(true, move |ports| {
                    finance_use_cases::list_payouts(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.pool.open" => {
                let input: finance_use_cases::OpenPoolInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.pool.open input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::open_pool(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.pool.refresh" => {
                let input: finance_use_cases::RefreshPoolInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.pool.refresh input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::refresh_pool(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.pool.close" => {
                let input: finance_use_cases::ClosePoolInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.pool.close input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::close_pool(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.pool.reserve" => {
                let input: finance_use_cases::ReservePoolInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.pool.reserve input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::reserve_pool(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.pool.settle" => {
                let input: finance_use_cases::SettlePoolInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.pool.settle input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::settle_pool(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.pool.release" => {
                let input: finance_use_cases::ReleasePoolInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.pool.release input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::release_pool(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.pool.debit" => {
                let input: finance_use_cases::DebitPoolInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.pool.debit input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::debit_pool(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.reconcile" => {
                let input: finance_use_cases::ReconcileFinancialSystemInput =
                    serde_json::from_slice(body)
                        .map_err(|_error| PortError::Unavailable("bad finance.reconcile input"))?;
                let caller = subject.id.to_string();
                self.in_finance(true, move |ports| {
                    finance_use_cases::reconcile_financial_system(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.adjustment" => {
                let input: finance_use_cases::PaymentAdjustmentInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.adjustment input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::payment_adjustment(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.transfer" => {
                let input: finance_use_cases::TransferInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.transfer input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::transfer(&ports, &caller, input.clone())
                        .map_err(legacy_error)
                })?
            }
            "finance.mint" => {
                let input: finance_use_cases::MintInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad finance.mint input"))?;
                let caller = subject.id.to_string();
                self.in_finance(false, move |ports| {
                    finance_use_cases::mint(&ports, &caller, input.clone()).map_err(legacy_error)
                })?
            }
            "entity.deploy" => {
                let input: crate::api::packets::program::DeployInput = serde_json::from_slice(body)
                    .map_err(|_error| PortError::Unavailable("bad entity.deploy input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    program_actions::serve_deploy_entity(&app, trx, &caller, input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "entity.download" => {
                let input: crate::api::packets::program::DownloadEntityInput =
                    serde_json::from_slice(body)
                        .map_err(|_error| PortError::Unavailable("bad entity.download input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(true, move |trx| {
                    program_actions::serve_download_entity(&app, trx, &caller, input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "entity.delete" => {
                let input: crate::api::packets::program::RunProgramEntityInput =
                    serde_json::from_slice(body)
                        .map_err(|_error| PortError::Unavailable("bad entity.delete input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    program_actions::serve_delete_program_entity(&app, trx, &caller, input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "workload.start" => {
                let input: crate::api::packets::program::RunProgramEntityInput =
                    serde_json::from_slice(body)
                        .map_err(|_error| PortError::Unavailable("bad workload.start input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    program_actions::serve_run_program_entity(&app, trx, &caller, input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "workload.stop" => {
                let input: crate::api::packets::program::RunProgramEntityInput =
                    serde_json::from_slice(body)
                        .map_err(|_error| PortError::Unavailable("bad workload.stop input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    program_actions::serve_stop_program_entity(&app, trx, &caller, input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "workload.list" => {
                let input: crate::api::packets::program::RunProgramEntityInput =
                    serde_json::from_slice(body)
                        .map_err(|_error| PortError::Unavailable("bad workload.list input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(true, move |trx| {
                    program_actions::serve_list_entity_vms(&app, trx, &caller, input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "workload.logs.read" => {
                let input: crate::api::packets::program::ReadVmLogsInput =
                    serde_json::from_slice(body)
                        .map_err(|_error| PortError::Unavailable("bad workload.logs.read input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(true, move |trx| {
                    program_actions::serve_read_vm_logs(&app, trx, &caller, input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "workload.terminal.open" => {
                let input: crate::api::packets::program::VmTerminalInput =
                    serde_json::from_slice(body).map_err(|_error| {
                        PortError::Unavailable("bad workload.terminal.open input")
                    })?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    program_actions::serve_open_vm_terminal(&app, trx, &caller, input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "workload.terminal.close" => {
                let input: crate::api::packets::program::VmTerminalInput =
                    serde_json::from_slice(body).map_err(|_error| {
                        PortError::Unavailable("bad workload.terminal.close input")
                    })?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    program_actions::serve_close_vm_terminal(&app, trx, &caller, input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "workload.builds.read" => {
                let input: crate::api::packets::program::MachineBuildsInput =
                    serde_json::from_slice(body).map_err(|_error| {
                        PortError::Unavailable("bad workload.builds.read input")
                    })?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(true, move |trx| {
                    program_actions::serve_read_machine_builds(&app, trx, &caller, input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "program.list" => {
                let input: crate::api::packets::program::ListInput =
                    serde_json::from_slice(body)
                        .map_err(|_error| PortError::Unavailable("bad program.list input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(true, move |trx| {
                    program_actions::serve_list_programs(&app, trx, &caller, input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "identity.session.create" => {
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(true, move |trx| {
                    creature_actions::serve_authenticate(&app, trx, &caller)
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "creature.types.read" => {
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(true, move |trx| {
                    creature_actions::serve_creature_types(&app, trx, &caller)
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "creature.signal" => {
                let input: crate::api::packets::creatures::SignalInput =
                    serde_json::from_slice(body)
                        .map_err(|_error| PortError::Unavailable("bad creature.signal input"))?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(true, move |trx| {
                    creature_actions::serve_creature_signal(&app, trx, &caller, "", input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "finance.lock.create" => {
                let input: crate::api::packets::creatures::LockTokenInput =
                    serde_json::from_slice(body).map_err(|_error| {
                        PortError::Unavailable("bad finance.lock.create input")
                    })?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    creature_actions::serve_lock_token(&app, trx, &caller, input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "finance.lock.consume" => {
                let input: crate::api::packets::creatures::ConsumeLockInput =
                    serde_json::from_slice(body).map_err(|_error| {
                        PortError::Unavailable("bad finance.lock.consume input")
                    })?;
                let app = self.app.clone();
                let caller = subject.id.to_string();
                self.in_trx(false, move |trx| {
                    creature_actions::serve_consume_lock(&app, trx, &caller, input.clone())
                        .map_err(|error| anyhow!("{error}"))
                })?
            }
            "identity.signature.check" => {
                return Err(PortError::Unsupported(
                    "action has no migrated public executor yet",
                ));
            }
            _ => {
                return Err(PortError::Unsupported(
                    "action has no migrated public executor yet",
                ));
            }
        };
        serde_json::to_vec(&output).map_err(|_error| PortError::Unavailable("encode failed"))
    }
}

/// Resolve a legacy session token to its subject through the node's session store.
struct LegacySessionDirectory {
    app: Arc<dyn ICore>,
}

impl SessionDirectory for LegacySessionDirectory {
    fn subject(&self, token: &str) -> Result<Option<Subject>, PortError> {
        let token_owned = token.to_string();
        let slot = Arc::new(std::sync::Mutex::new(None));
        let holder = slot.clone();
        self.app.modify_state(
            true,
            Box::new(move |trx: &dyn crate::models::transaction::ITrx| {
                let session = crate::api::model::session::Session {
                    id: token_owned.clone(),
                    ..Default::default()
                }
                .pull(trx);
                *holder.lock().unwrap() = Some(session.user_id);
                Ok(())
            }),
        );

        let user_id = slot
            .lock()
            .unwrap()
            .clone()
            .ok_or(PortError::Unavailable("session lookup failed"))?;
        if user_id.is_empty() {
            return Ok(None);
        }
        Ok(Some(creature_subject(&user_id)))
    }
}

/// The composed service that keeps the shared PostgreSQL repository alive for the
/// capsule-backed port adapters it owns.
struct ComposedPublicHttp {
    _repository: &'static PostgresCapsuleRepository,
    service: ComposedPublicActionService,
    realtime: Arc<PostgresRealtime>,
}

struct NodeFederationExecutor {
    actions: Arc<dyn ActionExecutor>,
}

impl FederationExecutor for NodeFederationExecutor {
    fn execute(&self, envelope: &Envelope, payload: &[u8]) -> Result<String, PortError> {
        let subject = envelope
            .subject
            .parse::<Subject>()
            .map_err(|_| PortError::Denied("invalid federation subject"))?;
        let answer = self.actions.execute(subject, &envelope.action, payload)?;
        String::from_utf8(answer)
            .map_err(|_| PortError::Failed("federated answer is not UTF-8".to_owned()))
    }
}

struct NodeFederationSigner(Ed25519KeyPair);

impl FederationResponseSigner for NodeFederationSigner {
    fn sign(&self, response: &[u8]) -> Result<String, PortError> {
        Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(self.0.sign(response).as_ref()))
    }
}

impl PublicActionService for ComposedPublicHttp {
    fn invoke(
        &self,
        request: aseman_public_http::PublicActionRequest,
    ) -> Result<PublicActionResponse, PublicActionError> {
        self.service.invoke(request)
    }
}

struct AuthorizedPublicEvents {
    realtime: Arc<dyn EventLog>,
    creature_id: aseman_domain::Uuid,
    stream: String,
}

fn public_event_error(status: u16, reason: &str, detail: impl Into<String>) -> PublicActionError {
    PublicActionError {
        status,
        reason: reason.to_owned(),
        detail: detail.into(),
    }
}

impl PublicEventSubscription for AuthorizedPublicEvents {
    fn read(
        &self,
        after_sequence: u64,
        limit: usize,
    ) -> Result<PublicEventBatch, PublicActionError> {
        let (latest, oldest) = self
            .realtime
            .bounds(&self.stream)
            .map_err(|error| public_event_error(503, "realtime_unavailable", error.to_string()))?;
        if !can_replay_from(oldest, after_sequence) {
            return Ok(PublicEventBatch::Resync {
                oldest_sequence: oldest,
                latest_sequence: latest,
            });
        }
        let publications = self
            .realtime
            .read(&self.stream, after_sequence, limit)
            .map_err(|error| public_event_error(503, "realtime_unavailable", error.to_string()))?;
        let mut frames = Vec::with_capacity(publications.len());
        for publication in publications {
            if !may_deliver(&publication.event, self.creature_id) {
                return Err(public_event_error(
                    403,
                    "event_scope_denied",
                    "the stream contains an event outside the admitted creature scope",
                ));
            }
            let event_id = publication.event.id.to_string();
            let sequence = publication.event.sequence;
            let kind = publication.event.kind.clone();
            let data = serde_json::to_string(&json!({
                "event": publication.event,
                "payloadBase64": base64::engine::general_purpose::STANDARD
                    .encode(publication.payload),
            }))
            .map_err(|error| public_event_error(500, "event_encode_failed", error.to_string()))?;
            frames.push(PublicEventFrame {
                event_id,
                sequence,
                kind,
                data,
            });
        }
        Ok(PublicEventBatch::Events(frames))
    }
}

impl PublicEventService for ComposedPublicHttp {
    fn subscribe(
        &self,
        request: PublicEventRequest,
    ) -> Result<Arc<dyn PublicEventSubscription>, PublicActionError> {
        let body = serde_json::to_vec(&json!({
            "token": request.token,
            "topics": [request.stream.clone()],
        }))
        .map_err(|error| {
            public_event_error(500, "subscription_encode_failed", error.to_string())
        })?;
        let response = self.service.invoke(PublicActionRequest {
            request_id: request.request_id,
            route: "/v1/actions/gateway/subscribe".to_owned(),
            action: "topic.subscribe".to_owned(),
            class: aseman_domain::authority::ActionClass::Read,
            authentication: request.authentication,
            idempotency_key: None,
            body,
        })?;
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|error| public_event_error(503, "subscription_invalid", error.to_string()))?;
        let creature_id = value
            .get("creatureId")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(|value| creature_subject(value).id)
            .ok_or_else(|| {
                public_event_error(
                    503,
                    "subscription_invalid",
                    "topic admission returned no creatureId",
                )
            })?;
        Ok(Arc::new(AuthorizedPublicEvents {
            realtime: self.realtime.clone(),
            creature_id,
            stream: request.stream,
        }))
    }
}

struct VmmLogTerminal {
    remote: Arc<crate::api::workloads::RemoteWorkloads>,
    workload: aseman_domain::WorkloadId,
}

impl PublicTerminalSession for VmmLogTerminal {
    fn read(
        &self,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<PublicTerminalOutput>, PublicActionError> {
        let records = self
            .remote
            .logs(self.workload, after_sequence)
            .map_err(|error| public_event_error(503, "terminal_unavailable", error.to_string()))?;
        Ok(records
            .into_iter()
            .take(limit.min(100))
            .map(|record| PublicTerminalOutput {
                sequence: record.sequence,
                channel: match record.stream {
                    aseman_domain::vmm::LogStream::Stdout => "stdout",
                    aseman_domain::vmm::LogStream::Stderr => "stderr",
                    aseman_domain::vmm::LogStream::System => "system",
                    aseman_domain::vmm::LogStream::Build => "build",
                }
                .to_owned(),
                data: record.line.into_bytes(),
            })
            .collect())
    }

    fn write(&self, _data: &[u8]) -> Result<(), PublicActionError> {
        Err(public_event_error(
            501,
            "unsupported_operation",
            "the installed runtime exposes the legacy log terminal, not interactive stdin",
        ))
    }

    fn resize(&self, _columns: u32, _rows: u32) -> Result<(), PublicActionError> {
        Err(public_event_error(
            501,
            "unsupported_operation",
            "the installed runtime exposes the legacy log terminal, not a PTY",
        ))
    }

    fn close(&self) -> Result<(), PublicActionError> {
        Ok(())
    }
}

impl PublicTerminalService for ComposedPublicHttp {
    fn open(
        &self,
        request: PublicTerminalRequest,
    ) -> Result<Arc<dyn PublicTerminalSession>, PublicActionError> {
        let workload = request
            .workload_id
            .parse::<uuid::Uuid>()
            .ok()
            .map(aseman_domain::WorkloadId::from_uuid)
            .ok_or_else(|| {
                public_event_error(400, "invalid_terminal_target", "workload ID is not a UUID")
            })?;
        // The legacy terminal was a log subscription (ADR 0029). Admission therefore
        // uses the ordinary workload log action, which authenticates the caller and
        // proves ownership. Its response returns the resolved typed workload ID; the
        // supplied target must match it before any stream is opened.
        let body = serde_json::to_vec(&json!({
            "vmId": request.vm_id,
            "logType": "terminal",
            "offset": 0,
            "count": 1,
            "creatureId": request.creature_id,
        }))
        .map_err(|error| public_event_error(500, "terminal_encode_failed", error.to_string()))?;
        let response = self.service.invoke(PublicActionRequest {
            request_id: request.request_id,
            route: "/v1/actions/machines/readVmLogs".to_owned(),
            action: "workload.logs.read".to_owned(),
            class: aseman_domain::authority::ActionClass::Read,
            authentication: request.authentication,
            idempotency_key: Some(request.idempotency_key),
            body,
        })?;
        let response: Value = serde_json::from_slice(&response.body)
            .map_err(|error| public_event_error(503, "terminal_invalid", error.to_string()))?;
        if response.get("workloadId").and_then(Value::as_str) != Some(&request.workload_id) {
            return Err(public_event_error(
                403,
                "terminal_scope_denied",
                "the admitted VM does not resolve to the requested workload",
            ));
        }
        let remote = crate::api::workloads::remote().ok_or_else(|| {
            public_event_error(
                503,
                "terminal_unavailable",
                "the node has no configured VMM",
            )
        })?;
        Ok(Arc::new(VmmLogTerminal { remote, workload }))
    }
}

fn read_database_url(config: &AsemanConfig) -> Result<String> {
    let secret = config.database_url_secret.as_deref().ok_or_else(|| {
        anyhow!("ASEMAN_DATABASE_URL_SECRET is required for the public HTTP service")
    })?;
    aseman_config::read_secret_file(secret, 4096)
        .map_err(|error| anyhow!("cannot read database URL secret {secret}: {error}"))
}

fn transport_config(listener: &PublicHttpListenerConfig) -> PublicHttpConfig {
    PublicHttpConfig {
        max_body_bytes: listener.max_body_bytes,
        max_in_flight: listener.max_in_flight,
        request_timeout: Duration::from_millis(listener.request_timeout_millis),
        requests_per_window: listener.requests_per_window,
        rate_window: Duration::from_secs(listener.rate_window_seconds),
        max_rate_subjects: listener.max_rate_subjects,
        allowed_origins: listener.allowed_origins.iter().cloned().collect(),
        drain_timeout: Duration::from_secs(listener.drain_timeout_seconds),
    }
}

/// Start the public HTTP listener. A no-op when the `ASEMAN_PUBLIC_HTTP_*`
/// configuration is absent, so an unconfigured node boots exactly as before.
///
/// # Errors
///
/// Invalid TLS material, an unreadable database secret, or a bind failure.
pub(crate) fn start_public_http(config: &AsemanConfig, app: Arc<dyn ICore>) -> Result<()> {
    let listener = match PublicHttpListenerConfig::from_process() {
        Ok(listener) => listener,
        Err(_) => return Ok(()),
    };
    // The repository backs every capsule port and lives for the node's lifetime.
    // The capsule adapters borrow it, so one clone is leaked to 'static; the owned
    // Arc serves the ReplayGuard/PublicActionIdempotency impls, which are on the type.
    let database_url = read_database_url(config)?;
    let repository_owned = Arc::new(PostgresCapsuleRepository::connect(&database_url)?);
    let repository: &'static PostgresCapsuleRepository =
        &*Box::leak(Box::new(repository_owned.clone()));

    let registry = aseman_contracts::security::action_registry()
        .map_err(|error| anyhow!("cannot load the A402 registry: {error}"))?;
    let policy: Arc<dyn PolicyDecisionPort> = Arc::new(aseman_policy_native::RegistryPolicy::new(
        registry.clone(),
        "node-v1",
    ));
    let advertised_port = config.network.legacy_tcp_port.to_string();

    let keys: Arc<dyn KeyDirectory> = Arc::new(CapsuleKeyDirectory { repository });
    let replay: Arc<dyn ReplayGuard> = repository_owned.clone();
    let verifier: Arc<dyn IdentityVerifier> = Arc::new(NativeIdentityVerifier);
    let grants: Arc<dyn GrantStore> = Arc::new(CapsuleGrantStore { repository });
    let audit: Arc<dyn DecisionAudit> = Arc::new(CapsuleDecisionAudit { repository });
    let idempotency: Arc<dyn PublicActionIdempotency> = repository_owned.clone();
    let sessions: Arc<dyn SessionDirectory> = Arc::new(LegacySessionDirectory { app: app.clone() });
    let origin = if config.node.origin.is_empty() {
        "global".to_owned()
    } else {
        config.node.origin.clone()
    };
    // RL-011: finance journal records are offered for ordering through the Hashgraph
    // consensus provider. The provider is composed once in `load_inner` (its proxy
    // must be installed into a Babble engine to actually finalize; until then records
    // remain pending — never mis-ordered). Provider-specific properties (staking
    // thresholds, election timing, validator caps) are configured environment-style
    // via `ConsensusProvider::set`, so the core does not couple to one provider's
    // feature set. If no provider was composed, finance actions run without an
    // ordering service (the same no-op the node used before RL-011).
    let consensus: Option<Arc<dyn aseman_ports::consensus::ConsensusProvider>> =
        app.consensus_provider();
    let executor: Arc<dyn ActionExecutor> = Arc::new(PublicActionExecutor {
        app,
        registry,
        clock: SystemClock,
        advertised_port,
        origin,
        consensus,
    });

    start_federation_http(
        config,
        &database_url,
        keys.clone(),
        replay.clone(),
        verifier.clone(),
        policy.clone(),
        executor.clone(),
    )?;

    let service = ComposedPublicActionService::new(
        keys,
        replay,
        sessions,
        verifier,
        Arc::new(SystemClock),
        policy,
        grants,
        audit,
        idempotency,
        executor,
        VerifierPolicy {
            audience: listener.audience.clone(),
            freshness: aseman_domain::identity::FreshnessPolicy::GUEST,
            rotation: aseman_domain::identity::RotationPolicy::DEFAULT,
        },
    );
    let realtime = Arc::new(
        PostgresRealtime::connect(&database_url, 4)
            .map_err(|error| anyhow!("cannot connect public realtime provider: {error}"))?,
    );
    realtime
        .migrate()
        .map_err(|error| anyhow!("cannot migrate public realtime provider: {error}"))?;
    let event_log: Arc<dyn EventLog> = realtime.clone();
    gateway_subs::configure_event_log(event_log);
    let composed = Arc::new(ComposedPublicHttp {
        _repository: repository,
        service,
        realtime,
    });
    let actions: Arc<dyn PublicActionService> = composed.clone();
    let events: Arc<dyn PublicEventService> = composed.clone();
    let terminals: Arc<dyn PublicTerminalService> = composed;

    // A702 is the sole application gateway for independently installed network
    // modules. A703 binds the candidate before committing its generation and keeps
    // the listener broker alive for drain/rollback. Plaintext is intentionally
    // loopback-only; an off-host module must be fronted by a mutually authenticated
    // transport endpoint.
    if let Some(endpoint) = listener.gateway_rpc_listen.clone() {
        let address = endpoint
            .parse::<std::net::SocketAddr>()
            .map_err(|error| anyhow!("invalid A702 listener {endpoint}: {error}"))?;
        let generation = listener.gateway_rpc_generation;
        if generation == 0 {
            return Err(anyhow!("ASEMAN_GATEWAY_RPC_GENERATION must be positive"));
        }
        let gateway_actions = actions.clone();
        let gateway_events = events.clone();
        let gateway_terminals = terminals.clone();
        let instance_id = config.node.id.clone();
        std::thread::Builder::new()
            .name("aseman-gateway-rpc".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .expect("gateway runtime");
                runtime.block_on(async move {
                    let broker = aseman_gateway_rpc::GatewayListenerBroker::default();
                    let service = aseman_gateway_rpc::GatewayService::new(
                        gateway_actions,
                        gateway_events,
                        instance_id,
                    )
                    .with_terminal(gateway_terminals);
                    match broker.stage(generation, address, service).await {
                        Ok(bound) => {
                            if let Err(error) = broker.activate(generation) {
                                eprintln!("[gateway-rpc] activation failed: {error}");
                                return;
                            }
                            eprintln!(
                                "[gateway-rpc] serving A702 generation {generation} on {bound}"
                            );
                            std::future::pending::<()>().await;
                        }
                        Err(error) => eprintln!("[gateway-rpc] staging failed: {error}"),
                    }
                });
            })
            .map_err(|error| anyhow!("cannot spawn the A702 gateway server: {error}"))?;
    }

    let certificate_chain = load_certificate_chain(&listener.tls_certificate)?;
    let private_key = load_private_key(&listener.tls_key_secret)?;
    let config = transport_config(&listener);
    let shutdown = std::future::pending();

    std::thread::Builder::new()
        .name("aseman-public-http".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("tokio runtime");
            runtime.block_on(async move {
                let address = listener
                    .listen
                    .parse::<std::net::SocketAddr>()
                    .map_err(|e| e.to_string())?;
                let tcp = tokio::net::TcpListener::bind(address)
                    .await
                    .map_err(|e| e.to_string())?;
                eprintln!("[public-http] serving the A701 contract on {}", address);
                aseman_public_http::serve_with_streams(
                    tcp,
                    certificate_chain,
                    private_key,
                    actions,
                    aseman_public_http::PublicStreamServices {
                        events: Some(events),
                        terminals: Some(terminals),
                    },
                    config,
                    shutdown,
                )
                .await
            })
        })
        .map_err(|error| anyhow!("cannot spawn the public HTTP server: {error}"))?;

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn start_federation_http(
    config: &AsemanConfig,
    database_url: &str,
    keys: Arc<dyn KeyDirectory>,
    replay: Arc<dyn ReplayGuard>,
    verifier: Arc<dyn IdentityVerifier>,
    policy: Arc<dyn PolicyDecisionPort>,
    actions: Arc<dyn ActionExecutor>,
) -> Result<()> {
    let listener = match FederationListenerConfig::from_process() {
        Ok(listener) => listener,
        Err(_) => return Ok(()),
    };
    let node_id = crate::api::workloads::node_subject(&config.node.id).id;
    let database = database_url
        .parse::<postgres::Config>()
        .map_err(|error| anyhow!("invalid federation database URL: {error}"))?;
    let provider = Arc::new(
        PostgresFederation::connect_config(database, 8, node_id)
            .map_err(|error| anyhow!("cannot connect federation provider: {error}"))?,
    );
    provider
        .migrate()
        .map_err(|error| anyhow!("cannot migrate federation provider: {error}"))?;

    let signing_pem =
        aseman_config::read_secret_file(&listener.response_signing_key_secret, 64 * 1024)?;
    let signing_der =
        rustls_pemfile::pkcs8_private_keys(&mut std::io::Cursor::new(signing_pem.as_bytes()))
            .next()
            .transpose()
            .map_err(|error| anyhow!("invalid federation response key: {error}"))?
            .ok_or_else(|| anyhow!("federation response key secret holds no PKCS#8 key"))?;
    let signer = Ed25519KeyPair::from_pkcs8(signing_der.secret_pkcs8_der())
        .map_err(|_| anyhow!("federation response key is not Ed25519 PKCS#8"))?;

    let directory: Arc<dyn aseman_ports::federation::Directory> = provider.clone();
    let guard: Arc<dyn aseman_ports::federation::EnvelopeGuard> = provider;
    let service = Arc::new(FederationService {
        keys,
        replay,
        verifier,
        directory,
        guard,
        policy,
        clock: Arc::new(SystemClock),
        executor: Arc::new(NodeFederationExecutor { actions }),
        response_signer: Arc::new(NodeFederationSigner(signer)),
        node_id,
        audience: listener.audience.clone(),
    });
    let tls = FederationServerTls {
        certificate_chain: load_certificate_chain(&listener.tls_certificate)?,
        private_key: load_private_key(&listener.tls_key_secret)?,
        client_roots: load_certificate_chain(&listener.client_ca)?,
    };
    let address = listener
        .listen
        .parse::<std::net::SocketAddr>()
        .map_err(|error| anyhow!("invalid federation listener: {error}"))?;
    let transport = FederationHttpConfig {
        max_body_bytes: listener.max_body_bytes,
        drain_timeout: Duration::from_secs(listener.drain_timeout_seconds),
    };
    std::thread::Builder::new()
        .name("aseman-federation-http".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .expect("federation runtime");
            runtime.block_on(async move {
                let tcp = match tokio::net::TcpListener::bind(address).await {
                    Ok(tcp) => tcp,
                    Err(error) => {
                        eprintln!("[federation-http] bind failed: {error}");
                        return;
                    }
                };
                eprintln!("[federation-http] serving A705 on {address}");
                if let Err(error) = aseman_federation_http::serve(
                    tcp,
                    tls,
                    service,
                    transport,
                    std::future::pending(),
                )
                .await
                {
                    eprintln!("[federation-http] stopped: {error}");
                }
            });
        })
        .map_err(|error| anyhow!("cannot spawn federation HTTP server: {error}"))?;
    Ok(())
}

fn load_certificate_chain(path: &str) -> Result<Vec<CertificateDer<'static>>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("cannot read TLS certificate {}", path))?;
    let mut reader = std::io::Cursor::new(bytes);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<_, _>>()
        .map_err(|error| anyhow!("invalid TLS certificate: {error}"))?;
    if certs.is_empty() {
        return Err(anyhow!("no certificate found in {}", path));
    }
    Ok(certs)
}

fn load_private_key(secret_path: &str) -> Result<PrivateKeyDer<'static>> {
    let bytes = aseman_config::read_secret_file(secret_path, 4096)?;
    let mut reader = std::io::Cursor::new(bytes);
    rustls_pemfile::private_key(&mut reader)
        .map_err(|error| anyhow!("invalid TLS private key: {error}"))?
        .ok_or_else(|| anyhow!("no private key found in {}", secret_path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_domain::realtime::{Event, RetentionClass};
    use aseman_ports::PortResult;
    use aseman_ports::realtime::Publication;

    struct FakeEventLog {
        oldest: Option<u64>,
        latest: Option<u64>,
        publications: Vec<Publication>,
    }

    impl EventLog for FakeEventLog {
        fn append(&self, _publication: &Publication) -> PortResult<()> {
            Err(PortError::Unsupported("test log is read-only"))
        }

        fn read(&self, _stream: &str, after: u64, limit: usize) -> PortResult<Vec<Publication>> {
            Ok(self
                .publications
                .iter()
                .filter(|publication| publication.event.sequence > after)
                .take(limit)
                .cloned()
                .collect())
        }

        fn bounds(&self, _stream: &str) -> PortResult<(Option<u64>, Option<u64>)> {
            Ok((self.latest, self.oldest))
        }

        fn purge_expired(&self, _now_millis: i64) -> PortResult<u64> {
            Err(PortError::Unsupported("test log is read-only"))
        }
    }

    fn publication(creature_id: aseman_domain::Uuid, sequence: u64) -> Publication {
        Publication {
            event: Event {
                id: aseman_domain::Uuid::now_v7(),
                stream: "creature:events".to_owned(),
                creature_id,
                kind: "store.updated".to_owned(),
                producer: "test".to_owned(),
                sequence,
                at_millis: 1,
                payload_digest: format!("sha256:{}", "0".repeat(64)),
                retention: RetentionClass::Standard,
                version: "1".to_owned(),
                idempotency_key: None,
            },
            payload: br#"{"ok":true}"#.to_vec(),
        }
    }

    #[test]
    fn resource_id_extracts_known_keys() {
        assert_eq!(
            resource_id("creature", br#"{"creatureId":"1@node"}"#).as_deref(),
            Some("1@node")
        );
        assert_eq!(
            resource_id("store", br#"{"storeId":"s1"}"#).as_deref(),
            Some("s1")
        );
        assert_eq!(
            resource_id("node", br#"{"name":"x"}"#).as_deref(),
            Some("x")
        );
        assert_eq!(resource_id("node", br#"{}"#), None);
    }

    #[test]
    fn diagnostics_use_case_answers() {
        let diagnostics = Diagnostics {
            clock: &SystemClock,
            advertised_port: "8074",
        };
        assert_eq!(diagnostics.hello("world"), "hello world !");
        assert_eq!(diagnostics.ping(), "8074");
        assert!(diagnostics.time_millis() > 0);
    }

    #[test]
    fn public_events_replay_only_the_admitted_creature_scope() {
        let creature_id = aseman_domain::Uuid::now_v7();
        let subscription = AuthorizedPublicEvents {
            realtime: Arc::new(FakeEventLog {
                oldest: Some(1),
                latest: Some(1),
                publications: vec![publication(creature_id, 1)],
            }),
            creature_id,
            stream: "creature:events".to_owned(),
        };
        let PublicEventBatch::Events(frames) = subscription.read(0, 10).unwrap() else {
            panic!("expected event frames");
        };
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].sequence, 1);
        assert!(frames[0].data.contains("payloadBase64"));

        let denied = AuthorizedPublicEvents {
            realtime: Arc::new(FakeEventLog {
                oldest: Some(1),
                latest: Some(1),
                publications: vec![publication(aseman_domain::Uuid::now_v7(), 1)],
            }),
            creature_id,
            stream: "creature:events".to_owned(),
        };
        assert_eq!(denied.read(0, 10).unwrap_err().status, 403);
    }

    #[test]
    fn public_events_require_resync_when_retention_passed_the_cursor() {
        let creature_id = aseman_domain::Uuid::now_v7();
        let subscription = AuthorizedPublicEvents {
            realtime: Arc::new(FakeEventLog {
                oldest: Some(20),
                latest: Some(30),
                publications: Vec::new(),
            }),
            creature_id,
            stream: "creature:events".to_owned(),
        };
        assert_eq!(
            subscription.read(2, 10).unwrap(),
            PublicEventBatch::Resync {
                oldest_sequence: Some(20),
                latest_sequence: Some(30),
            }
        );
    }
}
