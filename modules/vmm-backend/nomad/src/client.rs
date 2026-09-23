//! The Nomad HTTP API, narrowed to what A601 needs.
//!
//! Aseman never bundles or downloads Nomad (ADR 0002): the operator supplies a
//! running cluster and this client speaks to it. Every call is scoped to one
//! namespace and carries the backend's token, so the blast radius of a stolen token
//! is that namespace's jobs.

use std::time::Duration;

use aseman_ports::{PortError, PortResult};
use reqwest::blocking::{Client, RequestBuilder};
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn failed(error: impl std::fmt::Display) -> PortError {
    PortError::Failed(error.to_string())
}

/// An allocation of an Aseman job, in the shape the mapping reads.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Allocation {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "JobID")]
    pub job_id: String,
    /// `pending`, `running`, `complete`, `failed`, `lost`, or `unknown`.
    pub client_status: String,
    #[serde(default)]
    pub client_description: String,
    #[serde(default)]
    pub create_index: u64,
    #[serde(default)]
    pub modify_index: u64,
    /// Set while Nomad is placing or replacing the allocation.
    #[serde(default)]
    pub desired_status: String,
    #[serde(default, rename = "NodeID")]
    pub node_id: String,
    /// Present on a full read; absent from the list summary.
    #[serde(default)]
    pub allocated_resources: Option<Value>,
    #[serde(default)]
    pub resources: Option<Value>,
}

/// A job as the list endpoint returns it, with its Aseman meta.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct JobSummary {
    #[serde(rename = "ID")]
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub stop: bool,
    #[serde(default)]
    pub meta: std::collections::BTreeMap<String, String>,
}

/// A worker node, as the nodes endpoint returns it.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Node {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub datacenter: String,
    pub status: String,
    #[serde(default)]
    pub scheduling_eligibility: String,
    #[serde(default)]
    pub drain: bool,
}

/// A Nomad cluster, at one namespace.
pub struct Nomad {
    base: String,
    namespace: String,
    token: Option<String>,
    http: Client,
}

impl Nomad {
    /// Talk to the Nomad API at `base` (`http://host:4646`), in `namespace`.
    ///
    /// # Errors
    ///
    /// When the HTTP client cannot be built.
    pub fn new(
        base: impl Into<String>,
        namespace: impl Into<String>,
        token: Option<String>,
        timeout: Duration,
    ) -> PortResult<Self> {
        Ok(Self {
            base: base.into().trim_end_matches('/').to_owned(),
            namespace: namespace.into(),
            token,
            http: Client::builder().timeout(timeout).build().map_err(failed)?,
        })
    }

    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    fn request(&self, method: Method, route: &str) -> RequestBuilder {
        let separator = if route.contains('?') { '&' } else { '?' };
        let builder = self.http.request(
            method,
            format!(
                "{}{route}{separator}namespace={}",
                self.base, self.namespace
            ),
        );
        match &self.token {
            Some(token) => builder.header("X-Nomad-Token", token),
            None => builder,
        }
    }

    /// The response body, or the port error Nomad's status maps to.
    fn send(&self, builder: RequestBuilder) -> PortResult<Option<String>> {
        let response = builder.send().map_err(|error| {
            PortError::Unavailable(if error.is_timeout() {
                "Nomad (timeout)"
            } else {
                "Nomad"
            })
        })?;
        let status = response.status();
        let body = response.text().map_err(failed)?;
        match status {
            StatusCode::NOT_FOUND => Ok(None),
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => {
                Err(PortError::Denied("the Nomad token does not allow this"))
            }
            status if status.is_success() => Ok(Some(body)),
            StatusCode::SERVICE_UNAVAILABLE | StatusCode::BAD_GATEWAY => {
                Err(PortError::Unavailable("Nomad"))
            }
            status => Err(failed(format!("Nomad answered {status}: {}", body.trim()))),
        }
    }

    fn get<T: DeserializeOwned>(&self, route: &str) -> PortResult<Option<T>> {
        self.send(self.request(Method::GET, route))?
            .map(|body| serde_json::from_str(&body).map_err(failed))
            .transpose()
    }

    /// Register or update a job. Nomad's register is a plain upsert, so repeating it
    /// with the same specification changes nothing.
    ///
    /// # Errors
    ///
    /// When Nomad refuses the job or is unreachable.
    pub fn register(&self, job: &impl Serialize) -> PortResult<()> {
        #[derive(Serialize)]
        struct Register<'a, J: Serialize> {
            #[serde(rename = "Job")]
            job: &'a J,
        }
        self.send(
            self.request(Method::POST, "/v1/jobs")
                .json(&Register { job }),
        )?
        .ok_or_else(|| failed("Nomad did not accept the job"))
        .map(|_| ())
    }

    /// The job, or `None` when Nomad does not have it.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or refuses the read.
    pub fn job(&self, id: &str) -> PortResult<Option<JobSummary>> {
        self.get(&format!("/v1/job/{id}"))
    }

    /// Every job whose ID starts with `prefix`, with its meta.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or refuses the read.
    pub fn jobs(&self, prefix: &str) -> PortResult<Vec<JobSummary>> {
        Ok(self
            .get::<Vec<JobSummary>>(&format!("/v1/jobs?prefix={prefix}&meta=true"))?
            .unwrap_or_default())
    }

    /// The job's allocations, newest first.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or refuses the read.
    pub fn allocations(&self, job: &str) -> PortResult<Vec<Allocation>> {
        let mut allocations: Vec<Allocation> = self
            .get(&format!("/v1/job/{job}/allocations"))?
            .unwrap_or_default();
        allocations.sort_by_key(|allocation| std::cmp::Reverse(allocation.create_index));
        Ok(allocations)
    }

    /// Stop the job; `purge` removes it and its history.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or refuses the write.
    pub fn stop(&self, id: &str, purge: bool) -> PortResult<()> {
        self.send(self.request(Method::DELETE, &format!("/v1/job/{id}?purge={purge}")))
            .map(|_| ())
    }

    /// One task's log stream from `offset` bytes, as plain text. An allocation whose
    /// client has not written the file yet reads as empty.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or refuses the read.
    pub fn logs(
        &self,
        allocation: &str,
        task: &str,
        stream: &str,
        offset: u64,
    ) -> PortResult<String> {
        let route = format!(
            "/v1/client/fs/logs/{allocation}?task={task}&type={stream}&origin=start&offset={offset}&plain=true"
        );
        match self.send(self.request(Method::GET, &route)) {
            Ok(body) => Ok(body.unwrap_or_default()),
            // A task that has not produced this stream yet has no file.
            Err(PortError::Failed(_)) => Ok(String::new()),
            Err(error) => Err(error),
        }
    }

    /// The allocation's resource statistics.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or refuses the read.
    pub fn stats(&self, allocation: &str) -> PortResult<Option<Value>> {
        self.get(&format!("/v1/client/allocation/{allocation}/stats"))
    }

    /// The allocation with its network addresses.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or refuses the read.
    pub fn allocation(&self, allocation: &str) -> PortResult<Option<Allocation>> {
        self.get(&format!("/v1/allocation/{allocation}"))
    }

    /// A file inside the allocation directory.
    ///
    /// Nomad's `cat` answers a missing path with a plain-text message and a success
    /// status, so the file is stat'd first: a message must never be served as
    /// content.
    ///
    /// # Errors
    ///
    /// [`PortError::NotFound`] when the path is not a readable file; otherwise when
    /// Nomad is unreachable or refuses the read.
    pub fn read_file(&self, allocation: &str, path: &str) -> PortResult<Vec<u8>> {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct FileInfo {
            #[serde(default)]
            is_dir: bool,
        }
        let info: FileInfo = self
            .get(&format!("/v1/client/fs/stat/{allocation}?path={path}"))?
            .ok_or(PortError::NotFound)?;
        if info.is_dir {
            return Err(PortError::NotFound);
        }
        self.send(self.request(
            Method::GET,
            &format!("/v1/client/fs/cat/{allocation}?path={path}"),
        ))?
        .map(String::into_bytes)
        .ok_or(PortError::NotFound)
    }

    /// Every worker node in the cluster.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or refuses the read.
    pub fn nodes(&self) -> PortResult<Vec<Node>> {
        Ok(self.get::<Vec<Node>>("/v1/nodes")?.unwrap_or_default())
    }

    /// One worker's allocations.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or refuses the read.
    pub fn node_allocations(&self, node: &str) -> PortResult<Vec<Allocation>> {
        Ok(self
            .get(&format!("/v1/node/{node}/allocations"))?
            .unwrap_or_default())
    }

    /// A POST with a JSON body, for the operator actions that have no read.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable or refuses the write.
    pub fn post(&self, route: &str, body: &impl Serialize) -> PortResult<Option<String>> {
        self.send(self.request(Method::POST, route).json(body))
    }

    /// Whether the cluster answers.
    ///
    /// # Errors
    ///
    /// When Nomad is unreachable.
    pub fn agent_version(&self) -> PortResult<String> {
        let body: Value = self
            .get("/v1/agent/self")?
            .ok_or(PortError::Unavailable("Nomad"))?;
        Ok(body["config"]["Version"]["Version"]
            .as_str()
            .or_else(|| body["config"]["Version"].as_str())
            .unwrap_or("unknown")
            .to_owned())
    }
}
