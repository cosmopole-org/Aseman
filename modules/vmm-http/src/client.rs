//! The node's A501 client (mutual TLS, blocking). Mutations carry the caller's
//! idempotency key and are retried under it after a transport failure or a retryable
//! problem, so a retry never repeats an effect.

use std::time::Duration;

use aseman_contracts::vmm::EndpointList;
use aseman_contracts::vmm::{
    Capabilities, CreateWorkload, DEADLINE_HEADER, IDEMPOTENCY_KEY,
    LifecycleCommand as WireCommand, Operation, Page, Problem, ProblemCode, UpdateSpec, Workload,
    WorkloadEvent,
};
use aseman_domain::vmm::{Endpoint, LogRecord, OperationRecord, WorkloadRecord, WorkloadSpec};
use aseman_domain::{Generation, OperationId, WorkloadId};
use aseman_ports::vmm::{
    BackendDescription, EventBatch, LifecycleCommand, NewWorkload, VmmClient, WorkloadFilter,
};
use aseman_ports::{PortError, PortResult};
use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::wire;

const ATTEMPTS: u32 = 4;

/// The client's TLS material, PEM encoded.
pub struct ClientTls {
    /// The roots the VMM's server certificate must chain to.
    pub server_roots_pem: Vec<u8>,
    /// The client certificate chain followed by its private key.
    pub identity_pem: Vec<u8>,
}

pub struct HttpVmmClient {
    base: String,
    http: Client,
    /// This node's identity as the VMM knows it; set on records read back.
    owner: String,
    /// How long a mutation may take end to end (`Aseman-Deadline`).
    deadline: Duration,
    now_millis: fn() -> i64,
}

fn failed(error: impl std::fmt::Display) -> PortError {
    PortError::Failed(error.to_string())
}

fn system_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// The port error for a problem.
fn port_error(problem: &Problem) -> PortError {
    match problem.code {
        ProblemCode::NotFound => PortError::NotFound,
        ProblemCode::ResourceVersionMismatch
        | ProblemCode::StaleGeneration
        | ProblemCode::AlreadyExists => PortError::Conflict,
        ProblemCode::Unauthenticated | ProblemCode::Forbidden => {
            PortError::Denied(problem.code.as_str())
        }
        ProblemCode::Unavailable
        | ProblemCode::RateLimited
        | ProblemCode::IdempotencyInProgress => PortError::Unavailable(problem.code.as_str()),
        ProblemCode::DeadlineExceeded => PortError::Deadline,
        ProblemCode::UnsupportedOperation => PortError::Unsupported(problem.code.as_str()),
        _ => PortError::Failed(format!(
            "{}: {}",
            problem.code.as_str(),
            problem.detail.as_deref().unwrap_or(&problem.title)
        )),
    }
}

impl HttpVmmClient {
    /// A client for the VMM at `base_url` (`https://…`, no trailing slash).
    ///
    /// # Errors
    ///
    /// Invalid TLS material.
    pub fn new(
        base_url: &str,
        tls: &ClientTls,
        owner: &str,
        deadline: Duration,
    ) -> PortResult<Self> {
        let identity = reqwest::Identity::from_pem(&tls.identity_pem).map_err(failed)?;
        let mut builder = Client::builder()
            .use_rustls_tls()
            .tls_built_in_root_certs(false)
            .https_only(true)
            .identity(identity)
            .timeout(deadline)
            .connect_timeout(Duration::from_secs(5));
        for root in reqwest::Certificate::from_pem_bundle(&tls.server_roots_pem).map_err(failed)? {
            builder = builder.add_root_certificate(root);
        }
        Ok(Self {
            base: base_url.trim_end_matches('/').to_owned(),
            http: builder.build().map_err(failed)?,
            owner: owner.to_owned(),
            deadline,
            now_millis: system_millis,
        })
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .header(
                DEADLINE_HEADER,
                ((self.now_millis)()
                    + i64::try_from(self.deadline.as_millis()).unwrap_or(i64::MAX / 2))
                .to_string(),
            )
    }

    /// Send, retrying transport failures and retryable problems. `build` makes a
    /// fresh request per attempt.
    fn send(&self, build: impl Fn() -> RequestBuilder, retry: bool) -> PortResult<Response> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let outcome = build().send();
            let again = match &outcome {
                Err(_) => true,
                Ok(response) => matches!(
                    response.status(),
                    StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS
                ),
            };
            if !(retry && again) || attempt >= ATTEMPTS {
                return outcome.map_err(|_| PortError::Unavailable("VMM"));
            }
            std::thread::sleep(Duration::from_millis(100 * u64::from(attempt * attempt)));
        }
    }

    fn decode<T: DeserializeOwned>(response: Response) -> PortResult<T> {
        let status = response.status();
        let body = response
            .bytes()
            .map_err(|_| PortError::Unavailable("VMM"))?;
        if status.is_success() {
            return serde_json::from_slice(&body).map_err(failed);
        }
        match serde_json::from_slice::<Problem>(&body) {
            Ok(problem) => Err(port_error(&problem)),
            Err(_) => Err(PortError::Failed(format!("VMM answered {status}"))),
        }
    }

    fn get<T: DeserializeOwned>(&self, path: &str) -> PortResult<T> {
        Self::decode(self.send(|| self.request(Method::GET, path), true)?)
    }

    fn mutate<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        key: &str,
    ) -> PortResult<T> {
        let body = body.map(serde_json::to_vec).transpose().map_err(failed)?;
        let response = self.send(
            || {
                let request = self
                    .request(method.clone(), path)
                    .header(IDEMPOTENCY_KEY, key);
                match &body {
                    Some(body) => request
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(body.clone()),
                    None => request,
                }
            },
            true,
        )?;
        Self::decode(response)
    }

    fn operation_record(&self, operation: Operation) -> OperationRecord {
        wire::operation_record(&self.owner, operation)
    }
}

/// An idempotency key must be 16 to 128 characters of `[A-Za-z0-9_-]`.
fn check_key(key: &str) -> PortResult<()> {
    let valid = (16..=128).contains(&key.len())
        && key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(PortError::Failed("invalid idempotency key".to_owned()))
    }
}

/// Parse an SSE body into events; stops at a `resync` event.
fn parse_events(owner: &str, text: &str) -> PortResult<EventBatch> {
    let mut events = Vec::new();
    for block in text.split("\n\n") {
        let mut kind = "";
        let mut data = String::new();
        for line in block.lines() {
            if let Some(value) = line.strip_prefix("event:") {
                kind = value.trim();
            } else if let Some(value) = line.strip_prefix("data:") {
                data.push_str(value.trim_start());
            }
        }
        match kind {
            "" => {}
            "resync" => {
                return Ok(EventBatch {
                    events,
                    resync: true,
                });
            }
            _ => {
                let event: WorkloadEvent = serde_json::from_str(&data).map_err(failed)?;
                events.push(wire::event_record(owner, event));
            }
        }
    }
    Ok(EventBatch {
        events,
        resync: false,
    })
}

impl VmmClient for HttpVmmClient {
    fn capabilities(&self) -> PortResult<BackendDescription> {
        let capabilities: Capabilities = self.get("/v1/capabilities")?;
        Ok(BackendDescription {
            name: capabilities.backend.name,
            version: capabilities.backend.version,
            contract: capabilities.api_version,
            runtimes: capabilities.runtimes,
        })
    }

    fn create(&self, workload: &NewWorkload, idempotency_key: &str) -> PortResult<OperationRecord> {
        check_key(idempotency_key)?;
        let body = CreateWorkload {
            id: *workload.id.as_uuid(),
            labels: workload.labels.clone(),
            spec: workload.spec.clone(),
            desired: workload.desired,
        };
        let operation: Operation =
            self.mutate(Method::POST, "/v1/workloads", Some(&body), idempotency_key)?;
        Ok(self.operation_record(operation))
    }

    fn workload(&self, id: WorkloadId) -> PortResult<Option<WorkloadRecord>> {
        match self.get::<Workload>(&format!("/v1/workloads/{id}")) {
            Ok(workload) => wire::workload_record(&self.owner, workload).map(Some),
            Err(PortError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn workloads(
        &self,
        filter: &WorkloadFilter,
        cursor: Option<&str>,
        limit: usize,
    ) -> PortResult<aseman_ports::vmm::Page<WorkloadRecord>> {
        let mut query = vec![format!("limit={}", limit.clamp(1, 500))];
        if let Some(cursor) = cursor {
            query.push(format!("cursor={cursor}"));
        }
        if let Some(creature) = filter.creature_id {
            query.push(format!("creature_id={creature}"));
        }
        if let Some(state) = filter.observed_state {
            let state = serde_json::to_value(state).map_err(failed)?;
            query.push(format!(
                "observed_state={}",
                state.as_str().unwrap_or_default()
            ));
        }
        let page: Page<Workload> = self.get(&format!("/v1/workloads?{}", query.join("&")))?;
        Ok(aseman_ports::vmm::Page {
            items: page
                .items
                .into_iter()
                .map(|workload| wire::workload_record(&self.owner, workload))
                .collect::<PortResult<_>>()?,
            next_cursor: page.next_cursor,
        })
    }

    fn command(
        &self,
        id: WorkloadId,
        command: LifecycleCommand,
        generation: Generation,
        idempotency_key: &str,
    ) -> PortResult<OperationRecord> {
        check_key(idempotency_key)?;
        let operation: Operation = match command {
            LifecycleCommand::Delete => self.mutate::<(), _>(
                Method::DELETE,
                &format!("/v1/workloads/{id}?generation={}", generation.get()),
                None,
                idempotency_key,
            )?,
            _ => {
                let verb = match command {
                    LifecycleCommand::Start => "start",
                    LifecycleCommand::Stop => "stop",
                    LifecycleCommand::Pause => "pause",
                    _ => "resume",
                };
                self.mutate(
                    Method::POST,
                    &format!("/v1/workloads/{id}/{verb}"),
                    Some(&WireCommand { generation }),
                    idempotency_key,
                )?
            }
        };
        Ok(self.operation_record(operation))
    }

    fn update_spec(
        &self,
        id: WorkloadId,
        spec: &WorkloadSpec,
        generation: Generation,
        idempotency_key: &str,
    ) -> PortResult<OperationRecord> {
        check_key(idempotency_key)?;
        let operation: Operation = self.mutate(
            Method::PUT,
            &format!("/v1/workloads/{id}/spec"),
            Some(&UpdateSpec {
                generation,
                spec: spec.clone(),
            }),
            idempotency_key,
        )?;
        Ok(self.operation_record(operation))
    }

    fn invoke(
        &self,
        id: WorkloadId,
        invocation: &str,
        idempotency_key: &str,
    ) -> PortResult<OperationRecord> {
        check_key(idempotency_key)?;
        let body: serde_json::Value = serde_json::from_str(invocation).map_err(failed)?;
        let operation: Operation = self.mutate(
            Method::POST,
            &format!("/v1/workloads/{id}/invocations"),
            Some(&body),
            idempotency_key,
        )?;
        Ok(self.operation_record(operation))
    }

    fn forward_http(
        &self,
        id: WorkloadId,
        request: &str,
        idempotency_key: &str,
    ) -> PortResult<String> {
        check_key(idempotency_key)?;
        let body: serde_json::Value = serde_json::from_str(request).map_err(failed)?;
        let response: serde_json::Value = self.mutate(
            Method::POST,
            &format!("/v1/workloads/{id}/http"),
            Some(&body),
            idempotency_key,
        )?;
        Ok(response.to_string())
    }

    fn operation(&self, id: OperationId) -> PortResult<Option<OperationRecord>> {
        match self.get::<Operation>(&format!("/v1/operations/{id}")) {
            Ok(operation) => Ok(Some(self.operation_record(operation))),
            Err(PortError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn events_after(&self, after: u64, limit: usize) -> PortResult<EventBatch> {
        let response = self.send(
            || {
                self.request(Method::GET, "/v1/events?follow=false")
                    .header("Last-Event-ID", after.to_string())
            },
            true,
        )?;
        if !response.status().is_success() {
            return Self::decode::<serde_json::Value>(response).map(|_| EventBatch {
                events: Vec::new(),
                resync: false,
            });
        }
        let text = response.text().map_err(|_| PortError::Unavailable("VMM"))?;
        let mut batch = parse_events(&self.owner, &text)?;
        batch.events.truncate(limit.max(1));
        Ok(batch)
    }

    fn exec(
        &self,
        id: WorkloadId,
        request: &str,
        idempotency_key: &str,
    ) -> PortResult<OperationRecord> {
        check_key(idempotency_key)?;
        let body: serde_json::Value = serde_json::from_str(request).map_err(failed)?;
        let operation: Operation = self.mutate(
            Method::POST,
            &format!("/v1/workloads/{id}/exec"),
            Some(&body),
            idempotency_key,
        )?;
        Ok(self.operation_record(operation))
    }

    fn build(&self, request: &str, idempotency_key: &str) -> PortResult<OperationRecord> {
        check_key(idempotency_key)?;
        let body: serde_json::Value = serde_json::from_str(request).map_err(failed)?;
        let operation: Operation =
            self.mutate(Method::POST, "/v1/builds", Some(&body), idempotency_key)?;
        Ok(self.operation_record(operation))
    }

    fn put_file(
        &self,
        id: WorkloadId,
        path: &str,
        bytes: &[u8],
        idempotency_key: &str,
    ) -> PortResult<()> {
        check_key(idempotency_key)?;
        let route = format!("/v1/workloads/{id}/files/{path}");
        let response = self.send(
            || {
                self.request(Method::PUT, &route)
                    .header(IDEMPOTENCY_KEY, idempotency_key)
                    .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                    .body(bytes.to_vec())
            },
            true,
        )?;
        if response.status().is_success() {
            return Ok(());
        }
        Self::decode::<serde_json::Value>(response).map(|_| ())
    }

    fn get_file(&self, id: WorkloadId, path: &str) -> PortResult<Vec<u8>> {
        let route = format!("/v1/workloads/{id}/files/{path}");
        let response = self.send(|| self.request(Method::GET, &route), true)?;
        if response.status().is_success() {
            return response
                .bytes()
                .map(|bytes| bytes.to_vec())
                .map_err(|_| PortError::Unavailable("VMM"));
        }
        Self::decode::<serde_json::Value>(response).map(|_| Vec::new())
    }

    fn endpoints(&self, id: WorkloadId) -> PortResult<Vec<Endpoint>> {
        let list: EndpointList = self.get(&format!("/v1/workloads/{id}/endpoints"))?;
        Ok(list.items)
    }

    fn verify(&self, runtime: &str, request: &str, idempotency_key: &str) -> PortResult<String> {
        check_key(idempotency_key)?;
        let body: serde_json::Value = serde_json::from_str(request).map_err(failed)?;
        let result: serde_json::Value = self.mutate(
            Method::POST,
            &format!("/v1/runtimes/{runtime}/verifications"),
            Some(&body),
            idempotency_key,
        )?;
        Ok(result.to_string())
    }

    fn logs(&self, id: WorkloadId, after: u64) -> PortResult<Vec<LogRecord>> {
        let route = format!("/v1/workloads/{id}/logs?follow=false");
        let response = self.send(
            || {
                self.request(Method::GET, &route)
                    .header("Last-Event-ID", after.to_string())
            },
            true,
        )?;
        if !response.status().is_success() {
            return Self::decode::<serde_json::Value>(response).map(|_| Vec::new());
        }
        let text = response.text().map_err(|_| PortError::Unavailable("VMM"))?;
        text.split("\n\n")
            .filter_map(|block| {
                block
                    .lines()
                    .find_map(|line| line.strip_prefix("data:"))
                    .map(str::trim_start)
            })
            .map(|data| serde_json::from_str::<LogRecord>(data).map_err(failed))
            .collect()
    }
}

#[cfg(test)]
mod tests;
