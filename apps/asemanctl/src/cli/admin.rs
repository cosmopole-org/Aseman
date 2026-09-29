//! Calls to an administration API served over the cluster's mutual TLS (the storage
//! cluster's listener, or the node's module administration): the operator's cluster
//! identity (`ASEMAN_CLUSTER_TLS_*`) and the token in a request header, never on a
//! command line.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use aseman_admin_http::MutualTls;
use aseman_config::CliConfig;
use serde_json::Value;

/// How long one administration request may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(35);

/// `method` `url` with an optional JSON `body`; returns the JSON answer.
///
/// # Errors
///
/// No cluster TLS identity, a failed request, a non-success status, or an answer
/// carrying an `error`.
pub(crate) fn call(
    config: &CliConfig,
    token: &str,
    method: &str,
    url: &str,
    body: Option<&Value>,
) -> Result<Value> {
    let files = config.cluster_tls.as_ref().ok_or_else(|| {
        anyhow!(
            "administration uses the cluster's mutual TLS: set ASEMAN_CLUSTER_TLS_CERTIFICATE, \
             ASEMAN_CLUSTER_TLS_KEY_SECRET, and ASEMAN_CLUSTER_TLS_CA"
        )
    })?;
    let client = MutualTls::load(files)
        .map_err(|error| anyhow!("cluster TLS: {error}"))?
        .blocking_http_client(REQUEST_TIMEOUT)
        .map_err(|error| anyhow!(error))?;
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .with_context(|| format!("invalid method {method}"))?;
    let mut request = client
        .request(method, url)
        .header("content-type", "application/json");
    if !token.is_empty() {
        request = request.header("x-aseman-cluster-token", token);
    }
    if let Some(body) = body {
        request = request.body(serde_json::to_vec(body)?);
    }
    let response = request
        .send()
        .with_context(|| format!("request to {url} failed"))?;
    let status = response.status();
    let text = response.text().unwrap_or_default();
    let parsed: Option<Value> = serde_json::from_str(text.trim()).ok();
    if let Some(error) = parsed
        .as_ref()
        .and_then(|value| value.get("error"))
        .and_then(Value::as_str)
    {
        bail!("{error}");
    }
    if !status.is_success() {
        bail!("request to {url} failed with {status}: {}", text.trim());
    }
    parsed.ok_or_else(|| anyhow!("unexpected response from {url}: {}", text.trim()))
}
