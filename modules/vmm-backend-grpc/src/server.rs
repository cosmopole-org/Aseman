//! Serve a [`VmmBackend`] over A504.

use std::sync::Arc;

use aseman_contracts::vmm_backend_v1 as wire;
use aseman_contracts::vmm_backend_v1::vmm_backend_server;
use aseman_domain::WorkloadId;
use aseman_domain::vmm::{OperationRecord, WorkloadRecord};
use aseman_ports::PortError;
use aseman_ports::vmm::VmmBackend;
use tonic::{Request, Response, Status};

use crate::convert;

/// A504 over any backend implementation.
#[derive(Clone)]
pub struct BackendService {
    backend: Arc<dyn VmmBackend>,
}

impl BackendService {
    #[must_use]
    pub fn new(backend: Arc<dyn VmmBackend>) -> Self {
        Self { backend }
    }

    /// The tonic service.
    #[must_use]
    pub fn into_server(self) -> vmm_backend_server::VmmBackendServer<Self> {
        vmm_backend_server::VmmBackendServer::new(self)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES)
    }

    /// Run `call` on a blocking thread: backends are synchronous.
    async fn blocking<T: Send + 'static>(
        &self,
        call: impl FnOnce(&dyn VmmBackend) -> Result<T, PortError> + Send + 'static,
    ) -> Result<T, PortError> {
        let backend = self.backend.clone();
        tokio::task::spawn_blocking(move || call(&*backend))
            .await
            .unwrap_or_else(|_| Err(PortError::Failed("the backend panicked".to_owned())))
    }
}

/// Files and results larger than this are refused.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

fn workload(bytes: &[u8]) -> Result<WorkloadRecord, PortError> {
    serde_json::from_slice(bytes)
        .map_err(|error| PortError::Failed(format!("invalid workload: {error}")))
}

#[tonic::async_trait]
impl vmm_backend_server::VmmBackend for BackendService {
    async fn describe(
        &self,
        _: Request<wire::DescribeRequest>,
    ) -> Result<Response<wire::DescribeResponse>, Status> {
        Ok(Response::new(
            match self.blocking(|backend| backend.describe()).await {
                Ok(description) => wire::DescribeResponse {
                    name: description.name,
                    version: description.version,
                    contract: description.contract,
                    runtimes: description
                        .runtimes
                        .iter()
                        .map(convert::capabilities)
                        .collect(),
                    error: None,
                },
                Err(error) => wire::DescribeResponse {
                    error: Some(convert::error(&error)),
                    ..Default::default()
                },
            },
        ))
    }

    async fn step(
        &self,
        request: Request<wire::StepRequest>,
    ) -> Result<Response<wire::StepResponse>, Status> {
        let request = request.into_inner();
        let outcome = self
            .blocking(move |backend| {
                let record = workload(&request.workload)?;
                backend.step(&record, convert::domain_action(request.action)?)
            })
            .await;
        Ok(Response::new(match outcome {
            Ok(observation) => wire::StepResponse {
                observation: Some(convert::observation(&observation)),
                error: None,
            },
            Err(error) => wire::StepResponse {
                observation: None,
                error: Some(convert::error(&error)),
            },
        }))
    }

    async fn observe_all(
        &self,
        _: Request<wire::ObserveAllRequest>,
    ) -> Result<Response<wire::ObserveAllResponse>, Status> {
        Ok(Response::new(
            match self.blocking(|backend| backend.observe_all()).await {
                Ok(instances) => wire::ObserveAllResponse {
                    instances: instances
                        .iter()
                        .map(|(id, observation)| wire::InstanceObservation {
                            workload_id: id.to_string(),
                            observation: Some(convert::observation(observation)),
                        })
                        .collect(),
                    error: None,
                },
                Err(error) => wire::ObserveAllResponse {
                    instances: Vec::new(),
                    error: Some(convert::error(&error)),
                },
            },
        ))
    }

    async fn run(
        &self,
        request: Request<wire::RunRequest>,
    ) -> Result<Response<wire::RunResponse>, Status> {
        let request = request.into_inner();
        let outcome = self
            .blocking(move |backend| {
                let record = if request.workload.is_empty() {
                    None
                } else {
                    Some(workload(&request.workload)?)
                };
                let operation: OperationRecord = serde_json::from_slice(&request.operation)
                    .map_err(|error| PortError::Failed(format!("invalid operation: {error}")))?;
                backend.run(record.as_ref(), &operation)
            })
            .await;
        Ok(Response::new(match outcome {
            Ok(result) => wire::RunResponse {
                result: result.into_bytes(),
                error: None,
            },
            Err(error) => wire::RunResponse {
                result: Vec::new(),
                error: Some(convert::error(&error)),
            },
        }))
    }

    async fn forward_http(
        &self,
        request: Request<wire::ForwardHttpRequest>,
    ) -> Result<Response<wire::ForwardHttpResponse>, Status> {
        let request = request.into_inner();
        let outcome = self
            .blocking(move |backend| {
                let record = workload(&request.workload)?;
                let body = String::from_utf8(request.request)
                    .map_err(|_| PortError::Failed("the request is not UTF-8".to_owned()))?;
                backend.forward_http(&record, &body)
            })
            .await;
        Ok(Response::new(match outcome {
            Ok(response) => wire::ForwardHttpResponse {
                response: response.into_bytes(),
                error: None,
            },
            Err(error) => wire::ForwardHttpResponse {
                response: Vec::new(),
                error: Some(convert::error(&error)),
            },
        }))
    }

    async fn put_file(
        &self,
        request: Request<wire::PutFileRequest>,
    ) -> Result<Response<wire::PutFileResponse>, Status> {
        let request = request.into_inner();
        let outcome = self
            .blocking(move |backend| {
                backend.put_file(
                    &workload(&request.workload)?,
                    &request.path,
                    &request.content,
                )
            })
            .await;
        Ok(Response::new(wire::PutFileResponse {
            error: outcome.err().as_ref().map(convert::error),
        }))
    }

    async fn get_file(
        &self,
        request: Request<wire::GetFileRequest>,
    ) -> Result<Response<wire::GetFileResponse>, Status> {
        let request = request.into_inner();
        let outcome = self
            .blocking(move |backend| backend.get_file(&workload(&request.workload)?, &request.path))
            .await;
        Ok(Response::new(match outcome {
            Ok(content) => wire::GetFileResponse {
                content,
                error: None,
            },
            Err(error) => wire::GetFileResponse {
                content: Vec::new(),
                error: Some(convert::error(&error)),
            },
        }))
    }

    async fn endpoints(
        &self,
        request: Request<wire::EndpointsRequest>,
    ) -> Result<Response<wire::EndpointsResponse>, Status> {
        let request = request.into_inner();
        let outcome = self
            .blocking(move |backend| backend.endpoints(&workload(&request.workload)?))
            .await;
        Ok(Response::new(match outcome {
            Ok(endpoints) => wire::EndpointsResponse {
                endpoints: endpoints.iter().map(convert::endpoint).collect(),
                error: None,
            },
            Err(error) => wire::EndpointsResponse {
                endpoints: Vec::new(),
                error: Some(convert::error(&error)),
            },
        }))
    }

    async fn usage(
        &self,
        request: Request<wire::UsageRequest>,
    ) -> Result<Response<wire::UsageResponse>, Status> {
        let request = request.into_inner();
        let outcome = self
            .blocking(move |backend| backend.usage(&workload(&request.workload)?))
            .await;
        Ok(Response::new(match outcome {
            Ok(usage) => wire::UsageResponse {
                usage: Some(convert::usage(&usage)),
                error: None,
            },
            Err(error) => wire::UsageResponse {
                usage: None,
                error: Some(convert::error(&error)),
            },
        }))
    }

    async fn logs(
        &self,
        request: Request<wire::LogsRequest>,
    ) -> Result<Response<wire::LogsResponse>, Status> {
        let request = request.into_inner();
        let outcome = self
            .blocking(move |backend| {
                backend.logs(
                    &workload(&request.workload)?,
                    request.after,
                    usize::try_from(request.limit.max(1)).unwrap_or(1),
                )
            })
            .await;
        Ok(Response::new(match outcome {
            Ok(records) => wire::LogsResponse {
                records: records.iter().map(convert::log).collect(),
                error: None,
            },
            Err(error) => wire::LogsResponse {
                records: Vec::new(),
                error: Some(convert::error(&error)),
            },
        }))
    }

    async fn verify(
        &self,
        request: Request<wire::VerifyRequest>,
    ) -> Result<Response<wire::VerifyResponse>, Status> {
        let request = request.into_inner();
        let outcome = self
            .blocking(move |backend| {
                let body = String::from_utf8(request.request)
                    .map_err(|_| PortError::Failed("the request is not UTF-8".to_owned()))?;
                backend.verify(&request.runtime, &body)
            })
            .await;
        Ok(Response::new(match outcome {
            Ok(result) => wire::VerifyResponse {
                result: result.into_bytes(),
                error: None,
            },
            Err(error) => wire::VerifyResponse {
                result: Vec::new(),
                error: Some(convert::error(&error)),
            },
        }))
    }
}

/// Parse a workload ID the backend reported.
///
/// # Errors
///
/// `Failed` when it is not a UUID.
pub fn workload_id(text: &str) -> Result<WorkloadId, PortError> {
    text.parse::<WorkloadId>()
        .map_err(|_| PortError::Failed("invalid workload id".to_owned()))
}
