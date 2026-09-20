//! Harmless reference implementation of the generated module contracts.
#![forbid(unsafe_code)]

use aseman_contracts::module_control_v1::module_control_server::ModuleControl;
use aseman_contracts::module_control_v1::{
    CancelRequest, CancelResponse, Handshake, HealthRequest, HealthResponse, LifecycleRequest,
    LifecycleResponse, ProtocolVersion,
};
use aseman_contracts::module_sample_v1::sample_server::Sample;
use aseman_contracts::module_sample_v1::{EchoRequest, EchoResponse};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tonic::{Request, Response, Status};

const MAX_ECHO_BYTES: usize = 4096;

#[derive(Clone, Default)]
pub struct SampleProvider {
    ready: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
    cancelled: Arc<Mutex<std::collections::BTreeSet<String>>>,
}

impl SampleProvider {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(true)),
            draining: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(Mutex::new(std::collections::BTreeSet::new())),
        }
    }

    fn response_state(&self) -> &'static str {
        if self.draining.load(Ordering::SeqCst) {
            "draining"
        } else {
            "ready"
        }
    }
}

#[tonic::async_trait]
impl ModuleControl for SampleProvider {
    async fn negotiate(&self, request: Request<Handshake>) -> Result<Response<Handshake>, Status> {
        let request = request.into_inner();
        let protocol = request
            .protocol
            .ok_or_else(|| Status::invalid_argument("protocol version is required"))?;
        if protocol.major != 1 || request.provider_kind != "sample" {
            return Err(Status::unimplemented("incompatible module contract"));
        }
        Ok(Response::new(Handshake {
            protocol: Some(ProtocolVersion { major: 1, minor: 0 }),
            provider_kind: "sample".to_owned(),
            implementation_version: env!("CARGO_PKG_VERSION").to_owned(),
            capabilities: vec!["sample.echo".to_owned(), "sample.health".to_owned()],
            max_message_bytes: 16 * 1024,
            schema_digests: Vec::new(),
            instance_id: "sample-provider".to_owned(),
        }))
    }

    async fn health(
        &self,
        _request: Request<HealthRequest>,
    ) -> Result<Response<HealthResponse>, Status> {
        Ok(Response::new(HealthResponse {
            live: true,
            ready: self.ready.load(Ordering::SeqCst) && !self.draining.load(Ordering::SeqCst),
            detail: self.response_state().to_owned(),
        }))
    }

    async fn stage(
        &self,
        _request: Request<LifecycleRequest>,
    ) -> Result<Response<LifecycleResponse>, Status> {
        self.draining.store(false, Ordering::SeqCst);
        self.ready.store(true, Ordering::SeqCst);
        Ok(Response::new(LifecycleResponse {
            routing_generation: 0,
            state: "ready".to_owned(),
            error: None,
        }))
    }

    async fn drain(
        &self,
        _request: Request<LifecycleRequest>,
    ) -> Result<Response<LifecycleResponse>, Status> {
        self.draining.store(true, Ordering::SeqCst);
        Ok(Response::new(LifecycleResponse {
            routing_generation: 0,
            state: "draining".to_owned(),
            error: None,
        }))
    }

    async fn restore(
        &self,
        _request: Request<LifecycleRequest>,
    ) -> Result<Response<LifecycleResponse>, Status> {
        self.draining.store(false, Ordering::SeqCst);
        self.ready.store(true, Ordering::SeqCst);
        Ok(Response::new(LifecycleResponse {
            routing_generation: 0,
            state: "ready".to_owned(),
            error: None,
        }))
    }

    async fn stop(
        &self,
        _request: Request<LifecycleRequest>,
    ) -> Result<Response<LifecycleResponse>, Status> {
        self.ready.store(false, Ordering::SeqCst);
        Ok(Response::new(LifecycleResponse {
            routing_generation: 0,
            state: "stopped".to_owned(),
            error: None,
        }))
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

#[tonic::async_trait]
impl Sample for SampleProvider {
    async fn echo(&self, request: Request<EchoRequest>) -> Result<Response<EchoResponse>, Status> {
        if self.draining.load(Ordering::SeqCst) {
            return Err(Status::unavailable("provider is draining"));
        }
        let request = request.into_inner();
        let metadata = request
            .meta
            .ok_or_else(|| Status::invalid_argument("request metadata is required"))?;
        if metadata.request_id.is_empty()
            || metadata.trace_id.is_empty()
            || metadata.deadline_unix_millis <= 0
            || metadata.cancellation_id.is_empty()
            || metadata.idempotency_key.is_empty()
        {
            return Err(Status::invalid_argument(
                "complete request metadata is required",
            ));
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Status::internal("system clock is before Unix epoch"))?
            .as_millis()
            .min(i64::MAX as u128) as i64;
        if metadata.deadline_unix_millis <= now {
            return Err(Status::deadline_exceeded("request deadline has elapsed"));
        }
        if self
            .cancelled
            .lock()
            .map_err(|_| Status::internal("cancellation registry unavailable"))?
            .contains(&metadata.cancellation_id)
        {
            return Err(Status::cancelled("request was cancelled"));
        }
        if request.value.len() > MAX_ECHO_BYTES {
            return Err(Status::resource_exhausted("echo value exceeds limit"));
        }
        Ok(Response::new(EchoResponse {
            value: request.value,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_contracts::module_control_v1::{CancelRequest, RequestMetadata};

    fn metadata(cancellation_id: &str, deadline_unix_millis: i64) -> RequestMetadata {
        RequestMetadata {
            request_id: "request-1".to_owned(),
            trace_id: "trace-1".to_owned(),
            deadline_unix_millis,
            cancellation_id: cancellation_id.to_owned(),
            idempotency_key: "idem-1".to_owned(),
        }
    }

    #[tokio::test]
    async fn provider_negotiates_health_echo_and_drain() {
        let provider = SampleProvider::new();
        let negotiated = provider
            .negotiate(Request::new(Handshake {
                protocol: Some(ProtocolVersion { major: 1, minor: 2 }),
                provider_kind: "sample".to_owned(),
                implementation_version: "test".to_owned(),
                capabilities: vec!["sample.echo".to_owned()],
                max_message_bytes: 4096,
                schema_digests: Vec::new(),
                instance_id: "test-client".to_owned(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(negotiated.protocol.unwrap().major, 1);

        let echo = provider
            .echo(Request::new(EchoRequest {
                meta: Some(metadata("cancel-1", i64::MAX)),
                value: "hello".to_owned(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(echo.value, "hello");

        provider
            .drain(Request::new(LifecycleRequest::default()))
            .await
            .unwrap();
        assert!(
            provider
                .echo(Request::new(EchoRequest::default()))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn provider_enforces_deadlines_and_cancellation() {
        let provider = SampleProvider::new();
        let expired = provider
            .echo(Request::new(EchoRequest {
                meta: Some(metadata("expired", 1)),
                value: "too-late".to_owned(),
            }))
            .await
            .unwrap_err();
        assert_eq!(expired.code(), tonic::Code::DeadlineExceeded);

        provider
            .cancel(Request::new(CancelRequest {
                cancellation_id: "cancelled".to_owned(),
            }))
            .await
            .unwrap();
        let cancelled = provider
            .echo(Request::new(EchoRequest {
                meta: Some(metadata("cancelled", i64::MAX)),
                value: "cancelled".to_owned(),
            }))
            .await
            .unwrap_err();
        assert_eq!(cancelled.code(), tonic::Code::Cancelled);
    }

    #[tokio::test]
    async fn generated_clients_cross_the_real_grpc_boundary() {
        use aseman_contracts::module_control_v1::module_control_client::ModuleControlClient;
        use aseman_contracts::module_control_v1::module_control_server::ModuleControlServer;
        use aseman_contracts::module_sample_v1::sample_client::SampleClient;
        use aseman_contracts::module_sample_v1::sample_server::SampleServer;
        use tonic::transport::Server;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let provider = SampleProvider::new();
        let server = tokio::spawn(async move {
            Server::builder()
                .add_service(ModuleControlServer::new(provider.clone()))
                .add_service(SampleServer::new(provider))
                .serve(address)
                .await
        });
        let endpoint = format!("http://{address}");
        let mut control = None;
        for _ in 0..20 {
            match ModuleControlClient::connect(endpoint.clone()).await {
                Ok(client) => {
                    control = Some(client);
                    break;
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
        let mut control = control.expect("sample gRPC server must start");
        let response = control
            .health(HealthRequest::default())
            .await
            .unwrap()
            .into_inner();
        assert!(response.ready);

        let mut sample = SampleClient::connect(endpoint).await.unwrap();
        let response = sample
            .echo(EchoRequest {
                meta: Some(RequestMetadata {
                    request_id: "wire-request".to_owned(),
                    trace_id: "wire-trace".to_owned(),
                    deadline_unix_millis: i64::MAX,
                    cancellation_id: "wire-cancel".to_owned(),
                    idempotency_key: "wire-idempotency".to_owned(),
                }),
                value: "over-the-wire".to_owned(),
            })
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.value, "over-the-wire");
        server.abort();
    }
}
