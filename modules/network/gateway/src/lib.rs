//! A702's generated gRPC server and A703's live listener-generation runtime.
//!
//! Network modules forward credentials to this boundary. They never authenticate a
//! credential or call application internals themselves. Listener candidates bind
//! before activation, previous generations drain, and rollback is generation-fenced.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use aseman_contracts::gateway_v1 as wire;
use aseman_contracts::gateway_v1::gateway_server;
use aseman_contracts::module_control_v1 as control;
use aseman_domain::authority::ActionClass;
use aseman_domain::listener::{ListenerBroker, ListenerTransitionError};
use aseman_public_http::{
    Authentication, PublicActionError, PublicActionRequest, PublicActionService, PublicEventBatch,
    PublicEventRequest, PublicEventService, PublicTerminalRequest, PublicTerminalService,
};
use futures_util::{Stream, StreamExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::{Request, Response, Status};

pub const PROTOCOL_MAJOR: u32 = 1;
pub const PROTOCOL_MINOR: u32 = 0;
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
enum GatewayRequestError {
    InvalidArgument(&'static str),
    Unauthenticated(&'static str),
    ResourceExhausted(&'static str),
}

impl From<GatewayRequestError> for Status {
    fn from(error: GatewayRequestError) -> Self {
        match error {
            GatewayRequestError::InvalidArgument(message) => Status::invalid_argument(message),
            GatewayRequestError::Unauthenticated(message) => Status::unauthenticated(message),
            GatewayRequestError::ResourceExhausted(message) => Status::resource_exhausted(message),
        }
    }
}

fn module_error(
    code: control::ErrorCode,
    message: impl Into<String>,
    request_id: impl Into<String>,
    retryable: bool,
) -> control::ModuleError {
    control::ModuleError {
        code: code as i32,
        message: message.into(),
        retryable,
        request_id: request_id.into(),
        details: Default::default(),
    }
}

fn error_code(status: u16) -> control::ErrorCode {
    match status {
        400 | 422 => control::ErrorCode::InvalidArgument,
        401 => control::ErrorCode::Unauthenticated,
        403 => control::ErrorCode::PermissionDenied,
        404 => control::ErrorCode::NotFound,
        409 => control::ErrorCode::Conflict,
        408 | 504 => control::ErrorCode::DeadlineExceeded,
        501 => control::ErrorCode::Unsupported,
        429 | 502 | 503 => control::ErrorCode::Unavailable,
        _ => control::ErrorCode::Internal,
    }
}

fn action_error(error: PublicActionError, request_id: &str) -> control::ModuleError {
    module_error(
        error_code(error.status),
        format!("{}: {}", error.reason, error.detail),
        request_id,
        matches!(error.status, 429 | 502 | 503 | 504),
    )
}

fn metadata(
    request: &wire::InvokeRequest,
) -> Result<&control::RequestMetadata, GatewayRequestError> {
    request
        .meta
        .as_ref()
        .ok_or(GatewayRequestError::InvalidArgument(
            "request metadata is required",
        ))
}

fn authentication(request: &wire::InvokeRequest) -> Result<Authentication, GatewayRequestError> {
    match request.credential.as_ref() {
        Some(wire::invoke_request::Credential::Session(session))
            if !session.token.is_empty() && session.token.len() <= 512 =>
        {
            Ok(Authentication::Session(session.token.clone()))
        }
        Some(wire::invoke_request::Credential::Proof(proof))
            if !proof.canonical_proof.is_empty() && proof.canonical_proof.len() <= 16 * 1024 =>
        {
            let proof: aseman_contracts::identity::SignedRequestProof =
                serde_json::from_slice(&proof.canonical_proof).map_err(|_| {
                    GatewayRequestError::Unauthenticated("canonical proof is malformed")
                })?;
            let proof = proof.parse().map_err(|_| {
                GatewayRequestError::Unauthenticated("canonical proof is malformed")
            })?;
            Ok(Authentication::Proof(Box::new(proof)))
        }
        Some(_) => Err(GatewayRequestError::Unauthenticated(
            "credential is malformed",
        )),
        None => Err(GatewayRequestError::Unauthenticated(
            "exactly one credential is required",
        )),
    }
}

fn class(value: i32) -> Result<ActionClass, GatewayRequestError> {
    match wire::ActionClass::try_from(value) {
        Ok(wire::ActionClass::Read) => Ok(ActionClass::Read),
        Ok(wire::ActionClass::Write) => Ok(ActionClass::Write),
        Ok(wire::ActionClass::Administrative) => Ok(ActionClass::Administrative),
        _ => Err(GatewayRequestError::InvalidArgument(
            "action class is required",
        )),
    }
}

fn public_request(
    request: wire::InvokeRequest,
) -> Result<PublicActionRequest, GatewayRequestError> {
    let meta = metadata(&request)?.clone();
    if meta.request_id.is_empty() || request.route.is_empty() || request.action.is_empty() {
        return Err(GatewayRequestError::InvalidArgument(
            "request ID, route, and action are required",
        ));
    }
    if request.body.len() > MAX_MESSAGE_BYTES {
        return Err(GatewayRequestError::ResourceExhausted(
            "request body is too large",
        ));
    }
    let authentication = authentication(&request)?;
    let action_class = class(request.action_class)?;
    let idempotency_key = (!meta.idempotency_key.is_empty()).then_some(meta.idempotency_key);
    Ok(PublicActionRequest {
        request_id: meta.request_id,
        route: request.route,
        action: request.action,
        class: action_class,
        authentication,
        idempotency_key,
        body: request.body,
    })
}

/// A702 over the same authenticated application services used by public HTTP/SSE.
#[derive(Clone)]
pub struct GatewayService {
    actions: Arc<dyn PublicActionService>,
    events: Arc<dyn PublicEventService>,
    terminals: Option<Arc<dyn PublicTerminalService>>,
    instance_id: String,
}

impl GatewayService {
    #[must_use]
    pub fn new(
        actions: Arc<dyn PublicActionService>,
        events: Arc<dyn PublicEventService>,
        instance_id: impl Into<String>,
    ) -> Self {
        Self {
            actions,
            events,
            terminals: None,
            instance_id: instance_id.into(),
        }
    }

    #[must_use]
    pub fn with_terminal(mut self, terminals: Arc<dyn PublicTerminalService>) -> Self {
        self.terminals = Some(terminals);
        self
    }

    #[must_use]
    pub fn into_server(self) -> gateway_server::GatewayServer<Self> {
        gateway_server::GatewayServer::new(self)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES)
    }
}

type EventStream = Pin<Box<dyn Stream<Item = Result<wire::EventFrame, Status>> + Send + 'static>>;
type TerminalStream =
    Pin<Box<dyn Stream<Item = Result<wire::TerminalFrame, Status>> + Send + 'static>>;

#[tonic::async_trait]
impl gateway_server::Gateway for GatewayService {
    type SubscribeStream = EventStream;
    type TerminalStream = TerminalStream;

    async fn negotiate(
        &self,
        request: Request<control::Handshake>,
    ) -> Result<Response<control::Handshake>, Status> {
        let request = request.into_inner();
        let version = request
            .protocol
            .ok_or_else(|| Status::failed_precondition("protocol version is required"))?;
        if version.major != PROTOCOL_MAJOR {
            return Err(Status::failed_precondition(
                "unsupported gateway protocol major",
            ));
        }
        if request.provider_kind != "client_network" {
            return Err(Status::failed_precondition(
                "gateway peers must be client_network modules",
            ));
        }
        Ok(Response::new(control::Handshake {
            protocol: Some(control::ProtocolVersion {
                major: PROTOCOL_MAJOR,
                minor: PROTOCOL_MINOR,
            }),
            provider_kind: "gateway".to_owned(),
            implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
            capabilities: {
                let mut capabilities = vec![
                    "invoke".to_owned(),
                    "subscribe.replay".to_owned(),
                    "cancel".to_owned(),
                ];
                if self.terminals.is_some() {
                    capabilities.push("terminal".to_owned());
                }
                capabilities
            },
            max_message_bytes: MAX_MESSAGE_BYTES as u64,
            schema_digests: Vec::new(),
            instance_id: self.instance_id.clone(),
        }))
    }

    async fn invoke(
        &self,
        request: Request<wire::InvokeRequest>,
    ) -> Result<Response<wire::InvokeResponse>, Status> {
        let request = public_request(request.into_inner())?;
        let request_id = request.request_id.clone();
        let actions = self.actions.clone();
        let outcome = tokio::task::spawn_blocking(move || actions.invoke(request))
            .await
            .map_err(|_| Status::internal("gateway action task stopped"))?;
        Ok(Response::new(match outcome {
            Ok(response) => wire::InvokeResponse {
                status: u32::from(response.status),
                content_type: "application/json".to_owned(),
                body: response.body,
                error: None,
            },
            Err(error) => wire::InvokeResponse {
                status: u32::from(error.status),
                content_type: "application/problem+json".to_owned(),
                body: Vec::new(),
                error: Some(action_error(error, &request_id)),
            },
        }))
    }

    async fn subscribe(
        &self,
        request: Request<wire::SubscribeRequest>,
    ) -> Result<Response<Self::SubscribeStream>, Status> {
        let request = request.into_inner();
        let admission = request
            .admission
            .ok_or_else(|| Status::invalid_argument("subscription admission is required"))?;
        let admitted = public_request(admission)?;
        let request_id = admitted.request_id.clone();
        if request.stream.is_empty() || request.stream.len() > 256 {
            return Err(Status::invalid_argument("stream must be 1-256 bytes"));
        }
        let token: serde_json::Value = serde_json::from_slice(&admitted.body)
            .map_err(|_| Status::invalid_argument("subscription body is malformed"))?;
        let token = token
            .get("token")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| Status::invalid_argument("subscription token is required"))?
            .to_owned();
        let max_events = usize::try_from(request.max_events.clamp(1, 1_000)).unwrap_or(1);
        let after = if request.after_event_id.is_empty() {
            0
        } else {
            request
                .after_event_id
                .parse::<u64>()
                .map_err(|_| Status::invalid_argument("event cursor must be a sequence"))?
        };
        let service = self.events.clone();
        let subscription = tokio::task::spawn_blocking(move || {
            service.subscribe(PublicEventRequest {
                request_id,
                authentication: admitted.authentication,
                stream: request.stream,
                token,
            })
        })
        .await
        .map_err(|_| Status::internal("gateway subscription task stopped"))?
        .map_err(|error| {
            Status::permission_denied(format!("{}: {}", error.reason, error.detail))
        })?;

        let stream = futures_util::stream::unfold(
            (subscription, after, max_events, false),
            |(subscription, after, remaining, finished)| async move {
                if finished || remaining == 0 {
                    return None;
                }
                let reader = subscription.clone();
                let batch =
                    tokio::task::spawn_blocking(move || reader.read(after, remaining.min(100)))
                        .await;
                let mut frames = Vec::new();
                let mut next_after = after;
                let mut next_remaining = remaining;
                let mut done = false;
                match batch {
                    Ok(Ok(PublicEventBatch::Events(events))) => {
                        if events.is_empty() {
                            return None;
                        }
                        for event in events {
                            next_after = event.sequence;
                            next_remaining = next_remaining.saturating_sub(1);
                            frames.push(Ok(wire::EventFrame {
                                event_id: event.event_id,
                                sequence: event.sequence,
                                content_type: "application/json".to_owned(),
                                body: event.data.into_bytes(),
                                error: None,
                            }));
                        }
                    }
                    Ok(Ok(PublicEventBatch::Resync { .. })) => {
                        done = true;
                        frames.push(Ok(wire::EventFrame {
                            error: Some(module_error(
                                control::ErrorCode::Conflict,
                                "event cursor predates retained history; resync is required",
                                "",
                                false,
                            )),
                            ..Default::default()
                        }));
                    }
                    Ok(Err(error)) => {
                        done = true;
                        frames.push(Ok(wire::EventFrame {
                            error: Some(action_error(error, "")),
                            ..Default::default()
                        }));
                    }
                    Err(_) => {
                        frames.push(Err(Status::internal("gateway subscription task stopped")))
                    }
                }
                Some((
                    futures_util::stream::iter(frames),
                    (subscription, next_after, next_remaining, done),
                ))
            },
        )
        .flatten();
        Ok(Response::new(Box::pin(stream)))
    }

    async fn terminal(
        &self,
        request: Request<tonic::Streaming<wire::TerminalFrame>>,
    ) -> Result<Response<Self::TerminalStream>, Status> {
        let terminals = self
            .terminals
            .clone()
            .ok_or_else(|| Status::unimplemented("no terminal service is composed"))?;
        let mut inbound = request.into_inner();
        let first = inbound
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("terminal open frame is required"))?;
        let open = match first.frame {
            Some(wire::terminal_frame::Frame::Open(open)) => open,
            _ => {
                return Err(Status::invalid_argument(
                    "first frame must be terminal open",
                ));
            }
        };
        let admission = open
            .admission
            .ok_or_else(|| Status::invalid_argument("terminal admission is required"))?;
        let body: serde_json::Value = serde_json::from_slice(&admission.body)
            .map_err(|_| Status::invalid_argument("terminal admission body is malformed"))?;
        let creature_id = body
            .get("creatureId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let vm_id = body
            .get("vmId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let admitted = public_request(admission)?;
        let idempotency_key = admitted
            .idempotency_key
            .clone()
            .ok_or_else(|| Status::invalid_argument("terminal idempotency key is required"))?;
        let request_id = admitted.request_id.clone();
        let workload_id = open.workload_id;
        let session = tokio::task::spawn_blocking(move || {
            terminals.open(PublicTerminalRequest {
                request_id,
                authentication: admitted.authentication,
                idempotency_key,
                workload_id,
                creature_id,
                vm_id,
            })
        })
        .await
        .map_err(|_| Status::internal("terminal admission task stopped"))?
        .map_err(|error| {
            Status::permission_denied(format!("{}: {}", error.reason, error.detail))
        })?;

        let (outbound, receiver) = tokio::sync::mpsc::channel(64);
        tokio::spawn(async move {
            let mut after = 0;
            let mut poll = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                tokio::select! {
                    _ = poll.tick() => {
                        let reader = session.clone();
                        match tokio::task::spawn_blocking(move || reader.read(after, 100)).await {
                            Ok(Ok(records)) => {
                                for record in records {
                                    after = after.max(record.sequence);
                                    let frame = wire::TerminalFrame {
                                        frame: Some(wire::terminal_frame::Frame::Output(
                                            wire::TerminalOutput {
                                                channel: record.channel,
                                                data: record.data,
                                            },
                                        )),
                                    };
                                    if outbound.send(Ok(frame)).await.is_err() {
                                        let _ = session.close();
                                        return;
                                    }
                                }
                            }
                            Ok(Err(error)) => {
                                let _ = outbound.send(Ok(wire::TerminalFrame {
                                    frame: Some(wire::terminal_frame::Frame::Error(action_error(error, ""))),
                                })).await;
                                let _ = session.close();
                                return;
                            }
                            Err(_) => return,
                        }
                    }
                    incoming = inbound.message() => {
                        let Ok(Some(frame)) = incoming else {
                            let _ = session.close();
                            return;
                        };
                        let result = match frame.frame {
                            Some(wire::terminal_frame::Frame::Input(input)) => session.write(&input.data),
                            Some(wire::terminal_frame::Frame::Resize(resize)) => {
                                session.resize(resize.columns, resize.rows)
                            }
                            Some(wire::terminal_frame::Frame::Exit(_)) => {
                                let _ = session.close();
                                return;
                            }
                            _ => Err(PublicActionError {
                                status: 400,
                                reason: "invalid_terminal_frame".to_owned(),
                                detail: "terminal is already open".to_owned(),
                            }),
                        };
                        if let Err(error) = result
                            && outbound.send(Ok(wire::TerminalFrame {
                                frame: Some(wire::terminal_frame::Frame::Error(action_error(error, ""))),
                            })).await.is_err()
                        {
                            let _ = session.close();
                            return;
                        }
                    }
                }
            }
        });
        Ok(Response::new(Box::pin(
            tokio_stream::wrappers::ReceiverStream::new(receiver),
        )))
    }

    async fn cancel(
        &self,
        request: Request<control::CancelRequest>,
    ) -> Result<Response<control::CancelResponse>, Status> {
        if request.get_ref().cancellation_id.is_empty() {
            return Err(Status::invalid_argument("cancellation ID is required"));
        }
        // Unary work is deadline-bound and subscriptions end when their RPC is
        // dropped. A cancellation for an unknown/already-finished request is safe.
        Ok(Response::new(control::CancelResponse { accepted: false }))
    }
}

struct StagedListener {
    listener: TcpListener,
    service: GatewayService,
}

struct RunningListener {
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), tonic::transport::Error>>,
}

/// The executable A703 broker. It binds candidates before changing routing state,
/// runs one active generation, and retains its predecessor for bounded drain/rollback.
pub struct GatewayListenerBroker {
    state: Mutex<ListenerBroker>,
    staged: Mutex<BTreeMap<u64, StagedListener>>,
    running: Mutex<BTreeMap<u64, RunningListener>>,
}

impl Default for GatewayListenerBroker {
    fn default() -> Self {
        Self {
            state: Mutex::new(ListenerBroker::default()),
            staged: Mutex::new(BTreeMap::new()),
            running: Mutex::new(BTreeMap::new()),
        }
    }
}

impl GatewayListenerBroker {
    /// Bind a loopback candidate. Off-host A702 requires a separately composed mTLS
    /// endpoint; plaintext is never exposed beyond this host.
    pub async fn stage(
        &self,
        generation: u64,
        endpoint: SocketAddr,
        service: GatewayService,
    ) -> Result<SocketAddr, String> {
        if !matches!(endpoint.ip(), IpAddr::V4(ip) if ip.is_loopback())
            && !matches!(endpoint.ip(), IpAddr::V6(ip) if ip.is_loopback())
        {
            return Err("A702 plaintext listeners must bind loopback".to_owned());
        }
        let listener = TcpListener::bind(endpoint)
            .await
            .map_err(|error| error.to_string())?;
        let bound = listener.local_addr().map_err(|error| error.to_string())?;
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stage(generation, bound.to_string())
            .map_err(|error| error.to_string())?;
        self.staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(generation, StagedListener { listener, service });
        Ok(bound)
    }

    pub fn fail_staged(&self, generation: u64) -> Result<(), ListenerTransitionError> {
        self.staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&generation);
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .fail_staged(generation)
    }

    pub fn activate(&self, generation: u64) -> Result<(), String> {
        let candidate = self
            .staged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&generation)
            .ok_or_else(|| format!("listener generation {generation} is not staged"))?;
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .activate(generation)
            .map_err(|error| error.to_string())?;
        let (shutdown, mut draining) = watch::channel(false);
        let incoming = TcpListenerStream::new(candidate.listener);
        let task = tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(candidate.service.into_server())
                .serve_with_incoming_shutdown(incoming, async move {
                    while draining.changed().await.is_ok() {
                        if *draining.borrow() {
                            break;
                        }
                    }
                })
                .await
        });
        self.running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(generation, RunningListener { shutdown, task });
        Ok(())
    }

    pub async fn finish_drain(&self, generation: u64) -> Result<(), String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finish_drain(generation)
            .map_err(|error| error.to_string())?;
        let running = {
            self.running
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&generation)
        };
        if let Some(running) = running {
            let _ = running.shutdown.send(true);
            running
                .task
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    pub fn rollback(&self, failed_generation: u64) -> Result<(), String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .rollback(failed_generation)
            .map_err(|error| error.to_string())?;
        if let Some(running) = self
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&failed_generation)
        {
            let _ = running.shutdown.send(true);
            running.task.abort();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_public_http::{PublicActionResponse, PublicEventFrame, PublicEventSubscription};

    struct Actions;

    impl PublicActionService for Actions {
        fn invoke(
            &self,
            request: PublicActionRequest,
        ) -> Result<PublicActionResponse, PublicActionError> {
            Ok(PublicActionResponse {
                status: 200,
                body: request.body,
            })
        }
    }

    struct Subscription;

    impl PublicEventSubscription for Subscription {
        fn read(
            &self,
            after_sequence: u64,
            _limit: usize,
        ) -> Result<PublicEventBatch, PublicActionError> {
            Ok(PublicEventBatch::Events(vec![PublicEventFrame {
                event_id: "event".to_owned(),
                sequence: after_sequence + 1,
                kind: "test".to_owned(),
                data: "{}".to_owned(),
            }]))
        }
    }

    struct Events;

    impl PublicEventService for Events {
        fn subscribe(
            &self,
            _: PublicEventRequest,
        ) -> Result<Arc<dyn PublicEventSubscription>, PublicActionError> {
            Ok(Arc::new(Subscription))
        }
    }

    fn service() -> GatewayService {
        GatewayService::new(Arc::new(Actions), Arc::new(Events), "node-one")
    }

    fn invoke() -> wire::InvokeRequest {
        wire::InvokeRequest {
            meta: Some(control::RequestMetadata {
                request_id: "request-one".to_owned(),
                ..Default::default()
            }),
            route: "/v1/actions/api/hello".to_owned(),
            action: "node.diagnostics.read".to_owned(),
            action_class: wire::ActionClass::Read as i32,
            credential: Some(wire::invoke_request::Credential::Session(
                wire::SessionCredential {
                    token: "session".to_owned(),
                },
            )),
            content_type: "application/json".to_owned(),
            body: b"{}".to_vec(),
        }
    }

    #[tokio::test]
    async fn negotiation_is_major_version_and_provider_kind_bound() {
        let answer = gateway_server::Gateway::negotiate(
            &service(),
            Request::new(control::Handshake {
                protocol: Some(control::ProtocolVersion { major: 1, minor: 4 }),
                provider_kind: "client_network".to_owned(),
                ..Default::default()
            }),
        )
        .await
        .unwrap()
        .into_inner();
        assert_eq!(answer.protocol.unwrap().major, 1);
        assert!(answer.capabilities.contains(&"invoke".to_owned()));
        assert!(
            gateway_server::Gateway::negotiate(
                &service(),
                Request::new(control::Handshake {
                    protocol: Some(control::ProtocolVersion { major: 2, minor: 0 }),
                    provider_kind: "client_network".to_owned(),
                    ..Default::default()
                }),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn unary_rpc_forwards_credentials_without_treating_them_as_authority() {
        let response = gateway_server::Gateway::invoke(&service(), Request::new(invoke()))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"{}");
        assert!(response.error.is_none());
    }
}
