//! The node's public HTTP gateway (A701) and its federation edge (A705).
//!
//! The transport (`aseman-public-http`) owns the TLS/HTTP edge; the composed action
//! service (`aseman-public-service`) owns A401 authentication, A402 authorization,
//! and durable idempotency. The node supplies the executor, which runs each
//! operation on the node's router, and the session directory. The listener starts
//! only when `ASEMAN_PUBLIC_HTTP_*` is configured.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use aseman_application::federation::{FederatedAction, SendFederatedRequest};
use aseman_application::identity::VerifierPolicy;
use aseman_capsule::audit::CapsuleDecisionAudit;
use aseman_capsule::auto::AutoCommit;
use aseman_capsule::capability::CapsuleGrantStore;
use aseman_capsule::federation::StorageFederation;
use aseman_capsule::identity::CapsuleKeyDirectory;
use aseman_capsule::realtime::StorageRealtime;
use aseman_config::{AsemanConfig, PublicHttpListenerConfig};
use aseman_domain::authority::{ActionRegistry, Condition, ResourceRef};
use aseman_domain::federation::{Envelope, FederationReply};
use aseman_domain::identity::Subject;
use aseman_domain::realtime::{can_replay_from, may_deliver};
use aseman_federation_http::{
    DescriptorHttpTransport, FederationClientConfig, FederationExecutor, FederationHttpConfig,
    FederationNodeCredential, FederationResponseSigner, FederationServerTls, FederationService,
    FederationTls, federation_audience,
};
use aseman_identity_native::NativeIdentityVerifier;
use aseman_ports::federation::{Directory, Transport};
use aseman_ports::realtime::EventLog;
use aseman_ports::{
    ActionCall, ActionExecutor, ClockPort, DecisionAudit, GrantStore, IdentityVerifier,
    KeyDirectory, PolicyDecisionPort, PortError, PublicActionIdempotency, ReplayGuard,
    SessionDirectory,
};
use aseman_public_http::{
    PublicActionError, PublicActionRequest, PublicActionResponse, PublicActionService,
    PublicEventBatch, PublicEventFrame, PublicEventRequest, PublicEventService,
    PublicEventSubscription, PublicHttpConfig, PublicTerminalOutput, PublicTerminalRequest,
    PublicTerminalService, PublicTerminalSession,
};
use aseman_public_service::ComposedPublicActionService;
use base64::Engine;
use ring::signature::Ed25519KeyPair;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::actions::{Caller, OperationError, Router};
use crate::node::Node;
use crate::workloads::vmm::{SystemClock, creature_subject};

/// The public contract's operations on the node's router (A701).
///
/// `resolve` derives the resource kind from the A402 registry and its id from the
/// request body (best effort), establishing `authenticated` plus `self` when the
/// subject is the resource. `execute` runs the addressed operation for the
/// creature the subject is; a workload operation on a workload homed on another
/// node goes to that node (A705).
struct RouterExecutor {
    router: Arc<Router>,
    registry: ActionRegistry,
    clock: SystemClock,
    federation: Option<Arc<NodeFederationOutbound>>,
}

struct NodeFederationOutbound {
    directory: Arc<dyn Directory>,
    transport: Arc<dyn Transport>,
    policy: Arc<dyn PolicyDecisionPort>,
    node_id: aseman_domain::Uuid,
}

fn resource_id(kind: &str, body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    let key: &[&str] = match kind {
        "creature" => &["id", "creatureId", "username", "userId", "name"],
        "store" => &["storeId", "id", "name"],
        "program" => &["programId", "id", "name"],
        "workload" => &["workloadId", "targetVmId", "vmId", "id", "name"],
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

impl RouterExecutor {
    /// The answer of the node a workload lives on, when the call targets a
    /// workload homed elsewhere (A705).
    fn federated_answer(&self, call: &ActionCall) -> Result<Option<Vec<u8>>, PortError> {
        let (subject, action, body) = (call.subject, call.action.as_str(), call.body.as_slice());
        let Some(outbound) = &self.federation else {
            return Ok(None);
        };
        let (target, facts) = self.resolve(&subject, action, body)?;
        if target.kind != "workload" {
            return Ok(None);
        }
        let Ok(workload_id) = target.id.parse::<aseman_domain::Uuid>() else {
            // A workload named by a non-UUID id is this node's own.
            return Ok(None);
        };
        let Some(workload) = outbound
            .directory
            .workload(workload_id, self.clock.unix_millis())?
        else {
            return Ok(None);
        };
        if workload.home_node == outbound.node_id {
            return Ok(None);
        }
        // One request identity across the caller's retries.
        let request_id = {
            let stable = call.idempotency_key.as_deref().unwrap_or(&call.request_id);
            let mut hasher = Sha256::new();
            hasher.update(b"aseman-federation-request-v1\0");
            hasher.update(outbound.node_id.as_bytes());
            hasher.update(subject.to_string().as_bytes());
            hasher.update([0]);
            hasher.update(action.as_bytes());
            hasher.update([0]);
            hasher.update(target.kind.as_bytes());
            hasher.update([0]);
            hasher.update(target.id.as_bytes());
            hasher.update([0]);
            hasher.update(stable.as_bytes());
            let digest = hasher.finalize();
            let mut bytes = [0_u8; 16];
            bytes.copy_from_slice(&digest[..16]);
            // RFC 9562 name-based UUID shape; the digest namespace above defines the
            // name and no UUID value is treated as authority.
            bytes[6] = (bytes[6] & 0x0f) | 0x50;
            bytes[8] = (bytes[8] & 0x3f) | 0x80;
            aseman_domain::Uuid::from_bytes(bytes)
        };
        let reply = SendFederatedRequest {
            directory: outbound.directory.as_ref(),
            transport: outbound.transport.as_ref(),
            policy: outbound.policy.as_ref(),
            clock: &self.clock,
            node_id: outbound.node_id,
        }
        .send(FederatedAction {
            request_id,
            subject,
            destination_node: workload.home_node,
            target,
            action,
            payload: body,
            facts,
        })?;
        match reply {
            FederationReply::Executed(answer) | FederationReply::Replayed(answer) => {
                Ok(Some(answer.into_bytes()))
            }
            FederationReply::Refused(reason) => Err(PortError::Failed(format!(
                "federation destination refused the action: {reason}"
            ))),
        }
    }

    /// The creature a subject is: the legacy creature whose record the subject's
    /// id names, or the subject itself when it names none.
    fn caller(&self, subject: &Subject) -> Result<Caller, PortError> {
        let node = self.router.node();
        let user_id = node
            .read(|trx| {
                crate::state::creature_ports::CreaturePorts { trx }
                    .legacy_id_of(subject.id)
                    .map_err(|error| anyhow!("{error}"))
            })
            .map_err(PortError::failed)?
            .unwrap_or_else(|| subject.id.to_string());
        Ok(Caller {
            user_id,
            store_id: String::new(),
            source: node.id(),
        })
    }
}

impl ActionExecutor for RouterExecutor {
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

    fn execute(&self, call: &ActionCall) -> Result<Vec<u8>, PortError> {
        if let Some(answer) = self.federated_answer(call)? {
            return Ok(answer);
        }
        let operation = self
            .router
            .operation(&call.operation)
            .or_else(|| self.router.operation_for_action(&call.action))
            .filter(|operation| operation.action == call.action)
            .ok_or(PortError::NotFound)?;
        let caller = self.caller(&call.subject)?;
        let output = self
            .router
            .execute(&caller, operation, &call.body, false)
            .map_err(|error| match error {
                OperationError::Invalid(message) | OperationError::Refused(message) => {
                    PortError::Refused(message)
                }
                OperationError::Unavailable(message) => PortError::Failed(message),
            })?;
        serde_json::to_vec(&output).map_err(|_| PortError::Unavailable("encode failed"))
    }
}

/// A session token's subject, through the node's session store.
struct NodeSessionDirectory {
    node: Arc<Node>,
}

impl SessionDirectory for NodeSessionDirectory {
    fn subject(&self, token: &str) -> Result<Option<Subject>, PortError> {
        let session = self
            .node
            .read(|trx| crate::state::session::Session::find(trx, token))
            .map_err(PortError::failed)?;
        Ok(session
            .map(|session| session.user_id)
            .filter(|user_id| !user_id.is_empty())
            .map(|user_id| creature_subject(&user_id)))
    }
}

/// The composed service that keeps the node's storage alive for the capsule-backed
/// port adapters it owns.
struct ComposedPublicHttp {
    node: Arc<Node>,
    service: ComposedPublicActionService,
    realtime: Arc<StorageRealtime>,
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
        let (resolved, _) = self.actions.resolve(&subject, &envelope.action, payload)?;
        if format!("{}:{}", resolved.kind, resolved.id) != envelope.target {
            return Err(PortError::Denied(
                "federation payload does not match the authorized target",
            ));
        }
        let answer = self.actions.execute(&ActionCall {
            subject,
            operation: String::new(),
            action: envelope.action.clone(),
            body: payload.to_vec(),
            request_id: envelope.request_id.to_string(),
            idempotency_key: None,
        })?;
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
    remote: Arc<crate::workloads::vmm::RemoteWorkloads>,
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
            "the installed runtime exposes the log terminal, not interactive stdin",
        ))
    }

    fn resize(&self, _columns: u32, _rows: u32) -> Result<(), PublicActionError> {
        Err(public_event_error(
            501,
            "unsupported_operation",
            "the installed runtime exposes the log terminal, not a PTY",
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
        // The terminal is a log subscription (ADR 0029). Admission therefore
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
        let remote = self.node.vmm().ok_or_else(|| {
            public_event_error(
                503,
                "terminal_unavailable",
                "the node has no configured VMM",
            )
        })?;
        Ok(Arc::new(VmmLogTerminal { remote, workload }))
    }
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
pub(crate) fn start_public_http(config: &AsemanConfig, router: Arc<Router>) -> Result<()> {
    let Some(listener) = config.public_http.clone() else {
        return Ok(());
    };
    // One transaction per port call, on the node's storage.
    let storage = router.node().tools().storage().storage();
    let repository = AutoCommit(storage.clone());
    let registry = aseman_contracts::security::action_registry()
        .map_err(|error| anyhow!("cannot load the A402 registry: {error}"))?;
    let policy: Arc<dyn PolicyDecisionPort> = Arc::new(aseman_policy_native::RegistryPolicy::new(
        registry.clone(),
        "node-v1",
    ));
    let keys: Arc<dyn KeyDirectory> = Arc::new(CapsuleKeyDirectory {
        repository: repository.clone(),
    });
    let replay: Arc<dyn ReplayGuard> = Arc::new(repository.clone());
    let verifier: Arc<dyn IdentityVerifier> = Arc::new(NativeIdentityVerifier);
    let grants: Arc<dyn GrantStore> = Arc::new(CapsuleGrantStore {
        repository: repository.clone(),
    });
    let audit: Arc<dyn DecisionAudit> = Arc::new(CapsuleDecisionAudit {
        repository: repository.clone(),
    });
    let idempotency: Arc<dyn PublicActionIdempotency> = Arc::new(repository);
    let node = router.node().clone();
    let sessions: Arc<dyn SessionDirectory> = Arc::new(NodeSessionDirectory { node: node.clone() });
    let federation = compose_federation_outbound(config, &storage, policy.clone())?;
    let executor: Arc<dyn ActionExecutor> = Arc::new(RouterExecutor {
        router,
        registry,
        clock: SystemClock,
        federation,
    });

    start_federation_http(
        config,
        &storage,
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
    let realtime = Arc::new(StorageRealtime::new(storage));
    let event_log: Arc<dyn EventLog> = realtime.clone();
    node.topics().configure_event_log(event_log);
    let composed = Arc::new(ComposedPublicHttp {
        node,
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

fn compose_federation_outbound(
    config: &AsemanConfig,
    storage: &aseman_storage::Storage,
    policy: Arc<dyn PolicyDecisionPort>,
) -> Result<Option<Arc<NodeFederationOutbound>>> {
    let Some(outbound) = config.federation_outbound.clone() else {
        return Ok(None);
    };
    let node_id = crate::workloads::vmm::node_subject(&config.node.id).id;
    let provider = Arc::new(StorageFederation::new(storage.clone(), node_id));
    let own = provider
        .own_node()
        .map_err(|error| anyhow!("cannot load this node's federation descriptor: {error}"))?;
    if own.node_id != node_id || own.revoked_epochs.contains(&own.key_epoch) {
        return Err(anyhow!(
            "this node's federation descriptor has the wrong identity or a revoked current epoch"
        ));
    }

    let server_roots_pem = std::fs::read(&outbound.server_ca)
        .with_context(|| format!("cannot read federation server CA {}", outbound.server_ca))?;
    let mut identity_pem = std::fs::read(&outbound.client_certificate).with_context(|| {
        format!(
            "cannot read federation client certificate {}",
            outbound.client_certificate
        )
    })?;
    identity_pem.push(b'\n');
    identity_pem.extend_from_slice(
        aseman_config::read_secret_file(&outbound.client_key_secret, 64 * 1024)?.as_bytes(),
    );
    let signing_pem =
        aseman_config::read_secret_file(&outbound.request_signing_key_secret, 64 * 1024)?;
    let signing_der =
        rustls_pemfile::pkcs8_private_keys(&mut std::io::Cursor::new(signing_pem.as_bytes()))
            .next()
            .transpose()
            .map_err(|error| anyhow!("invalid federation request key: {error}"))?
            .ok_or_else(|| anyhow!("federation request key secret holds no PKCS#8 key"))?;
    let transport = DescriptorHttpTransport::new(
        FederationTls {
            server_roots_pem,
            identity_pem,
        },
        FederationNodeCredential {
            node_id,
            key_epoch: own.key_epoch,
            signing_key_pkcs8: signing_der.secret_pkcs8_der().to_vec(),
        },
        FederationClientConfig {
            deadline: Duration::from_millis(outbound.deadline_millis),
            attempts: outbound.attempts,
            initial_backoff: Duration::from_millis(outbound.initial_backoff_millis),
            maximum_backoff: Duration::from_millis(outbound.maximum_backoff_millis),
            circuit_failure_threshold: outbound.circuit_failure_threshold,
            circuit_open_for: Duration::from_secs(outbound.circuit_open_seconds),
        },
    )
    .map_err(|error| anyhow!("cannot compose federation HTTP client: {error}"))?;
    if !own.keys.contains(&transport.descriptor_public_key()) {
        return Err(anyhow!(
            "the federation request signing key is absent from this node's current descriptor"
        ));
    }
    let transport: Arc<dyn Transport> = Arc::new(transport);
    let directory: Arc<dyn Directory> = provider;
    Ok(Some(Arc::new(NodeFederationOutbound {
        directory,
        transport,
        policy,
        node_id,
    })))
}

#[allow(clippy::too_many_arguments)]
fn start_federation_http(
    config: &AsemanConfig,
    storage: &aseman_storage::Storage,
    keys: Arc<dyn KeyDirectory>,
    replay: Arc<dyn ReplayGuard>,
    verifier: Arc<dyn IdentityVerifier>,
    policy: Arc<dyn PolicyDecisionPort>,
    actions: Arc<dyn ActionExecutor>,
) -> Result<()> {
    let Some(listener) = config.federation_listener.clone() else {
        return Ok(());
    };
    let node_id = crate::workloads::vmm::node_subject(&config.node.id).id;
    let expected_audience = federation_audience(node_id);
    if listener.audience != expected_audience {
        return Err(anyhow!(
            "ASEMAN_FEDERATION_HTTP_AUDIENCE must be {expected_audience} for this node"
        ));
    }
    let provider = Arc::new(StorageFederation::new(storage.clone(), node_id));

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
