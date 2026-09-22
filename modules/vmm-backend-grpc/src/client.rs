//! The VMM service's side of A504: the [`VmmBackend`] port over gRPC.

use std::time::Duration;

use aseman_contracts::vmm_backend_v1 as wire;
use aseman_contracts::vmm_backend_v1::vmm_backend_client::VmmBackendClient;
use aseman_domain::WorkloadId;
use aseman_domain::vmm::{
    Endpoint, LogRecord, Observation, OperationRecord, ReconcileAction, Usage, WorkloadRecord,
};
use aseman_ports::vmm::{BackendDescription, VmmBackend};
use aseman_ports::{PortError, PortResult};
use tonic::transport::{Channel, Endpoint as TonicEndpoint};

use crate::convert;
use crate::server::{MAX_MESSAGE_BYTES, workload_id};

/// A504 client. It owns a small runtime so the synchronous port can be called from
/// any thread that is not itself inside an async runtime.
pub struct GrpcBackend {
    runtime: tokio::runtime::Runtime,
    client: VmmBackendClient<Channel>,
    deadline: Duration,
}

/// Whether `url` names this host: plaintext A504 never leaves the machine.
fn loopback(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split('/').next().unwrap_or("");
    let host = host
        .strip_prefix('[')
        .and_then(|inner| inner.split_once(']').map(|(host, _)| host))
        .unwrap_or_else(|| host.rsplit_once(':').map_or(host, |(host, _)| host));
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

impl GrpcBackend {
    /// A client for the backend at `url` (`http://127.0.0.1:port`).
    ///
    /// # Errors
    ///
    /// `Denied` for an address off this host, `Unavailable` when the runtime cannot
    /// start.
    pub fn connect(url: &str, deadline: Duration) -> PortResult<Self> {
        if !loopback(url) {
            return Err(PortError::Denied(
                "a VMM backend is reached on this host only",
            ));
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| PortError::Unavailable("backend runtime"))?;
        let endpoint = TonicEndpoint::from_shared(url.to_owned())
            .map_err(|_| PortError::Failed("invalid backend URL".to_owned()))?
            .connect_timeout(Duration::from_secs(5))
            .timeout(deadline);
        let channel = {
            let _entered = runtime.enter();
            endpoint.connect_lazy()
        };
        Ok(Self {
            runtime,
            client: VmmBackendClient::new(channel)
                .max_decoding_message_size(MAX_MESSAGE_BYTES)
                .max_encoding_message_size(MAX_MESSAGE_BYTES),
            deadline,
        })
    }

    fn meta(&self) -> aseman_contracts::module_control_v1::RequestMetadata {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(0));
        convert::metadata(
            &uuid::Uuid::now_v7().to_string(),
            now + i64::try_from(self.deadline.as_millis()).unwrap_or(0),
        )
    }

    fn call<T>(
        &self,
        call: impl std::future::Future<Output = Result<tonic::Response<T>, tonic::Status>>,
    ) -> PortResult<T> {
        self.runtime
            .block_on(call)
            .map(tonic::Response::into_inner)
            .map_err(|status| match status.code() {
                tonic::Code::DeadlineExceeded => PortError::Deadline,
                _ => PortError::Unavailable("backend"),
            })
    }
}

fn check(error: Option<&aseman_contracts::module_control_v1::ModuleError>) -> PortResult<()> {
    match error {
        Some(error) => Err(convert::port_error(error)),
        None => Ok(()),
    }
}

fn encode(record: &WorkloadRecord) -> PortResult<Vec<u8>> {
    serde_json::to_vec(record).map_err(|error| PortError::Failed(error.to_string()))
}

fn text(bytes: Vec<u8>) -> PortResult<String> {
    String::from_utf8(bytes).map_err(|_| PortError::Failed("the backend sent non-UTF-8".to_owned()))
}

impl VmmBackend for GrpcBackend {
    fn describe(&self) -> PortResult<BackendDescription> {
        let mut client = self.client.clone();
        let response = self.call(client.describe(wire::DescribeRequest {
            meta: Some(self.meta()),
        }))?;
        check(response.error.as_ref())?;
        if response.contract != convert::CONTRACT {
            return Err(PortError::Unsupported("backend contract version"));
        }
        Ok(BackendDescription {
            name: response.name,
            version: response.version,
            contract: response.contract,
            runtimes: response
                .runtimes
                .into_iter()
                .map(convert::domain_capabilities)
                .collect(),
        })
    }

    fn step(&self, workload: &WorkloadRecord, action: ReconcileAction) -> PortResult<Observation> {
        let mut client = self.client.clone();
        let response = self.call(client.step(wire::StepRequest {
            meta: Some(self.meta()),
            workload: encode(workload)?,
            action: convert::action(action) as i32,
        }))?;
        check(response.error.as_ref())?;
        convert::domain_observation(response.observation)
    }

    fn observe_all(&self) -> PortResult<Vec<(WorkloadId, Observation)>> {
        let mut client = self.client.clone();
        let response = self.call(client.observe_all(wire::ObserveAllRequest {
            meta: Some(self.meta()),
        }))?;
        check(response.error.as_ref())?;
        response
            .instances
            .into_iter()
            .map(|instance| {
                Ok((
                    workload_id(&instance.workload_id)?,
                    convert::domain_observation(instance.observation)?,
                ))
            })
            .collect()
    }

    fn run(
        &self,
        workload: Option<&WorkloadRecord>,
        operation: &OperationRecord,
    ) -> PortResult<String> {
        let mut client = self.client.clone();
        let response = self.call(
            client.run(wire::RunRequest {
                meta: Some(self.meta()),
                workload: workload.map(encode).transpose()?.unwrap_or_default(),
                operation: serde_json::to_vec(operation)
                    .map_err(|error| PortError::Failed(error.to_string()))?,
            }),
        )?;
        check(response.error.as_ref())?;
        text(response.result)
    }

    fn forward_http(&self, workload: &WorkloadRecord, request: &str) -> PortResult<String> {
        let mut client = self.client.clone();
        let response = self.call(client.forward_http(wire::ForwardHttpRequest {
            meta: Some(self.meta()),
            workload: encode(workload)?,
            request: request.as_bytes().to_vec(),
        }))?;
        check(response.error.as_ref())?;
        text(response.response)
    }

    fn put_file(&self, workload: &WorkloadRecord, path: &str, bytes: &[u8]) -> PortResult<()> {
        let mut client = self.client.clone();
        let response = self.call(client.put_file(wire::PutFileRequest {
            meta: Some(self.meta()),
            workload: encode(workload)?,
            path: path.to_owned(),
            content: bytes.to_vec(),
        }))?;
        check(response.error.as_ref())
    }

    fn get_file(&self, workload: &WorkloadRecord, path: &str) -> PortResult<Vec<u8>> {
        let mut client = self.client.clone();
        let response = self.call(client.get_file(wire::GetFileRequest {
            meta: Some(self.meta()),
            workload: encode(workload)?,
            path: path.to_owned(),
        }))?;
        check(response.error.as_ref())?;
        Ok(response.content)
    }

    fn endpoints(&self, workload: &WorkloadRecord) -> PortResult<Vec<Endpoint>> {
        let mut client = self.client.clone();
        let response = self.call(client.endpoints(wire::EndpointsRequest {
            meta: Some(self.meta()),
            workload: encode(workload)?,
        }))?;
        check(response.error.as_ref())?;
        response
            .endpoints
            .into_iter()
            .map(convert::domain_endpoint)
            .collect()
    }

    fn usage(&self, workload: &WorkloadRecord) -> PortResult<Usage> {
        let mut client = self.client.clone();
        let response = self.call(client.usage(wire::UsageRequest {
            meta: Some(self.meta()),
            workload: encode(workload)?,
        }))?;
        check(response.error.as_ref())?;
        response
            .usage
            .map(convert::domain_usage)
            .ok_or_else(|| PortError::Failed("the backend sent no usage".to_owned()))
    }

    fn logs(
        &self,
        workload: &WorkloadRecord,
        after: u64,
        limit: usize,
    ) -> PortResult<Vec<LogRecord>> {
        let mut client = self.client.clone();
        let response = self.call(client.logs(wire::LogsRequest {
            meta: Some(self.meta()),
            workload: encode(workload)?,
            after,
            limit: u32::try_from(limit).unwrap_or(u32::MAX),
        }))?;
        check(response.error.as_ref())?;
        response
            .records
            .into_iter()
            .map(convert::domain_log)
            .collect()
    }

    fn verify(&self, runtime: &str, request: &str) -> PortResult<String> {
        let mut client = self.client.clone();
        let response = self.call(client.verify(wire::VerifyRequest {
            meta: Some(self.meta()),
            runtime: runtime.to_owned(),
            request: request.as_bytes().to_vec(),
        }))?;
        check(response.error.as_ref())?;
        text(response.result)
    }
}

#[cfg(test)]
mod tests;
