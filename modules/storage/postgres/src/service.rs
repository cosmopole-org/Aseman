use crate::{PostgresCapsuleRepository, PostgresStorageError};
use aseman_contracts::capsule::{
    CapsuleEnvelope, CapsuleId, CapsuleKind, CapsuleQuery, StorageCapability,
};
use aseman_contracts::capsule_provider_v1::capsule_storage_server::CapsuleStorage;
use aseman_contracts::capsule_provider_v1::{
    DescribeRequest, DescribeResponse, GetCapsuleRequest, GetCapsuleResponse, PutCapsuleRequest,
    PutCapsuleResponse, QueryCapsulesRequest, QueryCapsulesResponse,
};
use aseman_contracts::module_control_v1::module_control_server::ModuleControl;
use aseman_contracts::module_control_v1::{
    CancelRequest, CancelResponse, ErrorCode, Handshake, HealthRequest, HealthResponse,
    LifecycleRequest, LifecycleResponse, ModuleError, ProtocolVersion, RequestMetadata,
};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tonic::{Request, Response, Status};

const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

enum RequestFailure {
    Invalid(&'static str),
    Deadline,
    Cancelled,
    Unavailable(&'static str),
    Internal(&'static str),
}

impl RequestFailure {
    fn into_status(self) -> Status {
        match self {
            Self::Invalid(message) => Status::invalid_argument(message),
            Self::Deadline => Status::deadline_exceeded("request deadline has elapsed"),
            Self::Cancelled => Status::cancelled("request was cancelled"),
            Self::Unavailable(message) => Status::unavailable(message),
            Self::Internal(message) => Status::internal(message),
        }
    }
}

#[derive(Clone)]
pub struct PostgresStorageService {
    repository: Arc<PostgresCapsuleRepository>,
    ready: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
    cancelled: Arc<Mutex<BTreeSet<String>>>,
}

impl PostgresStorageService {
    #[must_use]
    pub fn new(repository: Arc<PostgresCapsuleRepository>) -> Self {
        Self {
            repository,
            ready: Arc::new(AtomicBool::new(true)),
            draining: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }

    fn metadata(
        &self,
        metadata: Option<RequestMetadata>,
    ) -> Result<RequestMetadata, RequestFailure> {
        if self.draining.load(Ordering::SeqCst) || !self.ready.load(Ordering::SeqCst) {
            return Err(RequestFailure::Unavailable(
                "storage provider is not accepting work",
            ));
        }
        let metadata = metadata.ok_or(RequestFailure::Invalid("request metadata is required"))?;
        if metadata.request_id.is_empty()
            || metadata.trace_id.is_empty()
            || metadata.deadline_unix_millis <= 0
            || metadata.cancellation_id.is_empty()
            || metadata.idempotency_key.is_empty()
        {
            return Err(RequestFailure::Invalid(
                "complete request metadata is required",
            ));
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| RequestFailure::Internal("system clock precedes the Unix epoch"))?
            .as_millis()
            .min(i64::MAX as u128) as i64;
        if metadata.deadline_unix_millis <= now {
            return Err(RequestFailure::Deadline);
        }
        if self
            .cancelled
            .lock()
            .map_err(|_| RequestFailure::Internal("cancellation registry unavailable"))?
            .contains(&metadata.cancellation_id)
        {
            return Err(RequestFailure::Cancelled);
        }
        Ok(metadata)
    }
}

#[tonic::async_trait]
impl CapsuleStorage for PostgresStorageService {
    async fn describe(
        &self,
        request: Request<DescribeRequest>,
    ) -> Result<Response<DescribeResponse>, Status> {
        let request = request.into_inner();
        let _ = self
            .metadata(request.meta)
            .map_err(RequestFailure::into_status)?;
        let capabilities = PostgresCapsuleRepository::capabilities();
        Ok(Response::new(DescribeResponse {
            provider_id: capabilities.provider_id,
            capabilities: capability_names(&capabilities.capabilities),
            max_query_limit: capabilities.max_query_limit,
            max_transaction_capsules: capabilities.max_transaction_capsules,
            error: None,
        }))
    }

    async fn put_capsule(
        &self,
        request: Request<PutCapsuleRequest>,
    ) -> Result<Response<PutCapsuleResponse>, Status> {
        let request = request.into_inner();
        let metadata = self
            .metadata(request.meta)
            .map_err(RequestFailure::into_status)?;
        if request.capsule_cbor.len() > MAX_MESSAGE_BYTES {
            return Err(Status::resource_exhausted("capsule exceeds message limit"));
        }
        let capsule = match CapsuleEnvelope::from_canonical_bytes(&request.capsule_cbor) {
            Ok(capsule) => capsule,
            Err(error) => {
                return Ok(Response::new(PutCapsuleResponse {
                    revision: 0,
                    error: Some(module_error(
                        PostgresStorageError::Invalid(error.to_string()),
                        &metadata.request_id,
                    )),
                }));
            }
        };
        let revision = capsule.revision;
        let repository = Arc::clone(&self.repository);
        let result = tokio::task::spawn_blocking(move || {
            repository.put(&capsule, request.expected_revision)
        })
        .await
        .map_err(|error| Status::internal(error.to_string()))?;
        Ok(Response::new(match result {
            Ok(()) => PutCapsuleResponse {
                revision,
                error: None,
            },
            Err(error) => PutCapsuleResponse {
                revision: 0,
                error: Some(module_error(error, &metadata.request_id)),
            },
        }))
    }

    async fn get_capsule(
        &self,
        request: Request<GetCapsuleRequest>,
    ) -> Result<Response<GetCapsuleResponse>, Status> {
        let request = request.into_inner();
        let metadata = self
            .metadata(request.meta)
            .map_err(RequestFailure::into_status)?;
        let id: [u8; 16] = match request.id.try_into() {
            Ok(id) => id,
            Err(_) => return Err(Status::invalid_argument("capsule ID must be 16 bytes")),
        };
        let repository = Arc::clone(&self.repository);
        let result = tokio::task::spawn_blocking(move || {
            repository.get(&CapsuleKind(request.kind), &CapsuleId(id))
        })
        .await
        .map_err(|error| Status::internal(error.to_string()))?;
        Ok(Response::new(match result {
            Ok(Some(capsule)) => GetCapsuleResponse {
                found: true,
                capsule_cbor: capsule
                    .canonical_bytes()
                    .map_err(|error| Status::internal(error.to_string()))?,
                error: None,
            },
            Ok(None) => GetCapsuleResponse {
                found: false,
                capsule_cbor: Vec::new(),
                error: None,
            },
            Err(error) => GetCapsuleResponse {
                found: false,
                capsule_cbor: Vec::new(),
                error: Some(module_error(error, &metadata.request_id)),
            },
        }))
    }

    async fn query_capsules(
        &self,
        request: Request<QueryCapsulesRequest>,
    ) -> Result<Response<QueryCapsulesResponse>, Status> {
        let request = request.into_inner();
        let metadata = self
            .metadata(request.meta)
            .map_err(RequestFailure::into_status)?;
        if request.query_json.len() > MAX_MESSAGE_BYTES {
            return Err(Status::resource_exhausted("query exceeds message limit"));
        }
        let query: CapsuleQuery = match serde_json::from_slice(&request.query_json) {
            Ok(query) => query,
            Err(error) => {
                return Ok(Response::new(QueryCapsulesResponse {
                    capsule_cbor: Vec::new(),
                    error: Some(module_error(
                        PostgresStorageError::Invalid(error.to_string()),
                        &metadata.request_id,
                    )),
                }));
            }
        };
        let repository = Arc::clone(&self.repository);
        let result = tokio::task::spawn_blocking(move || repository.query(&query))
            .await
            .map_err(|error| Status::internal(error.to_string()))?;
        Ok(Response::new(match result {
            Ok(capsules) => {
                let mut encoded = Vec::with_capacity(capsules.len());
                let mut total = 0_usize;
                for capsule in capsules {
                    let bytes = capsule
                        .canonical_bytes()
                        .map_err(|error| Status::internal(error.to_string()))?;
                    total = total.saturating_add(bytes.len());
                    if total > MAX_MESSAGE_BYTES {
                        return Err(Status::resource_exhausted(
                            "query response exceeds message limit",
                        ));
                    }
                    encoded.push(bytes);
                }
                QueryCapsulesResponse {
                    capsule_cbor: encoded,
                    error: None,
                }
            }
            Err(error) => QueryCapsulesResponse {
                capsule_cbor: Vec::new(),
                error: Some(module_error(error, &metadata.request_id)),
            },
        }))
    }
}

#[tonic::async_trait]
impl ModuleControl for PostgresStorageService {
    async fn negotiate(&self, request: Request<Handshake>) -> Result<Response<Handshake>, Status> {
        let request = request.into_inner();
        let protocol = request
            .protocol
            .ok_or_else(|| Status::invalid_argument("protocol version is required"))?;
        if protocol.major != 1 || request.provider_kind != "storage" {
            return Err(Status::unimplemented("incompatible module contract"));
        }
        let capabilities = PostgresCapsuleRepository::capabilities();
        Ok(Response::new(Handshake {
            protocol: Some(ProtocolVersion { major: 1, minor: 0 }),
            provider_kind: "storage".to_owned(),
            implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
            capabilities: capability_names(&capabilities.capabilities),
            max_message_bytes: MAX_MESSAGE_BYTES as u64,
            schema_digests: vec![
                contract_digest(include_bytes!(
                    "../../../../contracts/capsule/provider/v1/storage.proto"
                )),
                contract_digest(include_bytes!(
                    "../../../../contracts/storage/postgres/core-mapping.json"
                )),
            ],
            instance_id: "postgres-core-v1".to_owned(),
        }))
    }

    async fn health(
        &self,
        _request: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        let ready = self.ready.load(Ordering::SeqCst) && !self.draining.load(Ordering::SeqCst);
        Ok(Response::new(HealthResponse {
            live: true,
            ready,
            detail: if ready { "ready" } else { "not-ready" }.to_owned(),
        }))
    }

    async fn stage(
        &self,
        _request: Request<LifecycleRequest>,
    ) -> Result<Response<LifecycleResponse>, Status> {
        self.draining.store(false, Ordering::SeqCst);
        self.ready.store(true, Ordering::SeqCst);
        Ok(lifecycle("ready"))
    }

    async fn restore(
        &self,
        _request: Request<LifecycleRequest>,
    ) -> Result<Response<LifecycleResponse>, Status> {
        self.draining.store(false, Ordering::SeqCst);
        self.ready.store(true, Ordering::SeqCst);
        Ok(lifecycle("ready"))
    }

    async fn drain(
        &self,
        _request: Request<LifecycleRequest>,
    ) -> Result<Response<LifecycleResponse>, Status> {
        self.draining.store(true, Ordering::SeqCst);
        Ok(lifecycle("draining"))
    }

    async fn stop(
        &self,
        _request: Request<LifecycleRequest>,
    ) -> Result<Response<LifecycleResponse>, Status> {
        self.ready.store(false, Ordering::SeqCst);
        Ok(lifecycle("stopped"))
    }

    async fn cancel(
        &self,
        request: Request<CancelRequest>,
    ) -> Result<Response<CancelResponse>, Status> {
        let cancellation_id = request.into_inner().cancellation_id;
        if cancellation_id.is_empty() {
            return Ok(Response::new(CancelResponse { accepted: false }));
        }
        self.cancelled
            .lock()
            .map_err(|_| Status::internal("cancellation registry unavailable"))?
            .insert(cancellation_id);
        Ok(Response::new(CancelResponse { accepted: true }))
    }
}

fn capability_names(capabilities: &BTreeSet<StorageCapability>) -> Vec<String> {
    capabilities
        .iter()
        .map(|capability| capability.wire_name().to_owned())
        .collect()
}

fn contract_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn module_error(error: PostgresStorageError, request_id: &str) -> ModuleError {
    let (code, retryable) = match &error {
        PostgresStorageError::Invalid(_) => (ErrorCode::InvalidArgument, false),
        PostgresStorageError::Conflict => (ErrorCode::Conflict, false),
        PostgresStorageError::Unsupported(_) => (ErrorCode::Unsupported, false),
        PostgresStorageError::Unavailable(_) => (ErrorCode::Unavailable, true),
    };
    ModuleError {
        code: code as i32,
        message: error.to_string(),
        retryable,
        request_id: request_id.to_owned(),
        details: Default::default(),
    }
}

fn lifecycle(state: &str) -> Response<LifecycleResponse> {
    Response::new(LifecycleResponse {
        routing_generation: 0,
        state: state.to_owned(),
        error: None,
    })
}
