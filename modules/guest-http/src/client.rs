//! A backend's client for the node guest API: every request is signed with the
//! credential of the workload it is made for.

use std::time::Duration;

use aseman_contracts::guest_api::{
    ARTIFACT_ACTION, ARTIFACTS_PATH, CALLS_PATH, PROOF_HEADER, WorkloadCredential, call_action,
};
use aseman_ports::{PortError, PortResult};
use reqwest::StatusCode;
use reqwest::blocking::Client;

pub struct GuestApiClient {
    http: Client,
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

impl GuestApiClient {
    /// A client trusting `server_roots_pem` (a PEM bundle) for the node's certificate.
    /// It speaks https only.
    ///
    /// # Errors
    ///
    /// Invalid roots.
    pub fn new(server_roots_pem: &[u8], timeout: Duration) -> PortResult<Self> {
        let mut builder = Client::builder()
            .use_rustls_tls()
            .tls_built_in_root_certs(false)
            .https_only(true)
            .timeout(timeout);
        for root in reqwest::Certificate::from_pem_bundle(server_roots_pem)
            .map_err(|error| PortError::Failed(error.to_string()))?
        {
            builder = builder.add_root_certificate(root);
        }
        Ok(Self {
            http: builder
                .build()
                .map_err(|error| PortError::Failed(error.to_string()))?,
        })
    }

    fn send(
        &self,
        credential: &WorkloadCredential,
        method: reqwest::Method,
        path: &str,
        action: &str,
        resource: &str,
        body: &[u8],
    ) -> PortResult<Vec<u8>> {
        let proof = credential
            .sign(action, resource, body, now_millis())
            .map_err(|_| PortError::Denied("unusable workload credential"))?;
        let response = self
            .http
            .request(method, format!("{}{path}", credential.guest_api))
            .header(PROOF_HEADER, proof)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_vec())
            .send()
            .map_err(|_| PortError::Unavailable("guest API"))?;
        let status = response.status();
        let bytes = response
            .bytes()
            .map_err(|_| PortError::Unavailable("guest API"))?
            .to_vec();
        if status.is_success() {
            return Ok(bytes);
        }
        let detail = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .map(|problem| {
                format!(
                    "{} {}",
                    problem["code"].as_str().unwrap_or(""),
                    problem["detail"].as_str().unwrap_or("")
                )
                .trim()
                .to_owned()
            })
            .unwrap_or_default();
        Err(match status {
            StatusCode::NOT_FOUND => PortError::NotFound,
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                PortError::Failed(format!("denied: {detail}"))
            }
            StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS => {
                PortError::Unavailable("guest API")
            }
            _ => PortError::Failed(detail),
        })
    }

    /// Run host call `op` with JSON `input` as the credential's workload.
    ///
    /// # Errors
    ///
    /// `Unsupported` for an unregistered call; `Unavailable` on transport failure;
    /// `Failed` with the node's reason otherwise.
    pub fn call(
        &self,
        credential: &WorkloadCredential,
        op: &str,
        input: &[u8],
    ) -> PortResult<Vec<u8>> {
        let action = call_action(op).ok_or(PortError::Unsupported("unregistered host call"))?;
        self.send(
            credential,
            reqwest::Method::POST,
            &format!("{CALLS_PATH}{op}"),
            action,
            op,
            input,
        )
    }

    /// Download the workload's program artifact with `digest`.
    ///
    /// # Errors
    ///
    /// As [`Self::call`].
    pub fn artifact(&self, credential: &WorkloadCredential, digest: &str) -> PortResult<Vec<u8>> {
        self.send(
            credential,
            reqwest::Method::GET,
            &format!("{ARTIFACTS_PATH}{digest}"),
            ARTIFACT_ACTION,
            digest,
            &[],
        )
    }
}

#[cfg(test)]
mod tests;
