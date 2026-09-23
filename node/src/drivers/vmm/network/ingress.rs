//! The VMM HTTP ingress server.
//!
//! A long-lived HTTP listener that accepts requests in either of two shapes:
//!
//! ```text
//! {caspar node instance url}/{creatureId}/{programId}/{entityId}/{vmId}/{path…}
//! {caspar node instance url}/{creatureUsername}/{customPath…}
//! ```
//!
//! and forwards them to the HTTP server of the targeted VM instance. The first
//! form names the instance directly by `vmId`; the second is a friendlier route
//! a deployer bound to a VM entity at deploy time (metadata `gatewayPath`) —
//! the leading segment is resolved as a creature username and the custom path
//! prefix is matched against the routes registered for that creature (see
//! [`crate::drivers::vmm::http_route`]).
//!
//! The ingress is a *pure HTTP adapter*: it parses the request, resolves the
//! identity segments (through the VMM for the custom-route form), and hands the
//! packaged request to its owning node
//! instance's VMM via `self.app.tools().workloads().forward_http(..)`. It never
//! reaches into the packet router or plugin registry itself, so it carries no
//! process-wide state and is scoped entirely to the `ICore` instance it was
//! constructed with. The VMM's `forward_http` resolves the entity's runtime
//! and dispatches to its plugin, where:
//!
//! * five of the six runtimes fall back to the SDK's generic
//!   the node's signal fallback (`driver::forward_as_signal`), which
//!   signals the VM so it handles the request on its next run, and
//! * the docker runtime overrides it to proxy the request straight to the HTTP
//!   server running inside the container and return its real response.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::drivers::vmm::prelude::*;
use crate::models::core::ICore;
use crate::models::ports::ratelimit::{Protocol, RateLimitDecision, RateLimitKey};

/// Maximum request body the ingress will buffer (16 MiB).
const MAX_BODY: usize = 16 * 1024 * 1024;

pub(crate) struct VmHttpIngress {
    app: Arc<dyn ICore>,
    listening: AtomicBool,
}

impl VmHttpIngress {
    pub(crate) fn new(app: Arc<dyn ICore>) -> Arc<VmHttpIngress> {
        Arc::new(VmHttpIngress {
            app,
            listening: AtomicBool::new(false),
        })
    }

    /// Start the HTTP listener on `0.0.0.0:port` (no-op when `port <= 0` or the
    /// listener is already running). Each accepted connection is served on its
    /// own thread.
    pub(crate) fn listen(self: &Arc<Self>, port: i64) {
        if port <= 0 {
            return;
        }
        if self.listening.swap(true, Ordering::AcqRel) {
            return;
        }
        let ingress = Arc::clone(self);
        thread::spawn(move || {
            let addr = format!("0.0.0.0:{}", port);
            let listener = match TcpListener::bind(&addr) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[vm-http-ingress] bind {} failed: {}", addr, e);
                    ingress.listening.store(false, Ordering::Release);
                    return;
                }
            };
            eprintln!("[vm-http-ingress] listening on {}", addr);
            for incoming in listener.incoming() {
                match incoming {
                    Ok(stream) => {
                        let g = Arc::clone(&ingress);
                        thread::spawn(move || g.handle_connection(stream));
                    }
                    Err(e) => {
                        eprintln!("[vm-http-ingress] accept error: {}", e);
                        continue;
                    }
                }
            }
        });
    }

    fn handle_connection(self: Arc<Self>, mut stream: TcpStream) {
        // Best-effort remote IP for rate-limit bucketing. The ingress is
        // unauthenticated, so every request is billed to its peer IP under the
        // anonymous tier of the shared cross-protocol limiter.
        let peer_ip = stream
            .peer_addr()
            .ok()
            .map(|a| a.ip().to_string())
            .unwrap_or_default();

        let req = match parse_request(&mut stream) {
            Ok(r) => r,
            Err(e) => {
                write_response(
                    &mut stream,
                    400,
                    "Bad Request",
                    "application/json",
                    None,
                    json!({"ok": false, "error": e}).to_string().into_bytes(),
                );
                return;
            }
        };

        let (status, reason, content_type, headers, body) = self.route(&req, &peer_ip);
        write_response(&mut stream, status, &reason, &content_type, headers, body);
    }

    /// Resolve, package, and forward a parsed request; returns the HTTP
    /// response tuple `(status, reason, content_type, extra_headers, body)`.
    fn route(
        &self,
        req: &HttpRequest,
        peer_ip: &str,
    ) -> (u16, String, String, Option<Vec<(String, String)>>, Vec<u8>) {
        // Cross-protocol admission control. The HTTP ingress is anonymous, so
        // requests are billed to the peer IP under the shared limiter's
        // anonymous tier — the same instance the TCP/WS transports use, so a
        // client cannot dodge its quota by switching to HTTP. On rejection we
        // answer a standards-compliant 429 with a `Retry-After` header.
        let rl_key = RateLimitKey::anonymous(Protocol::Http, peer_ip, &req.path);
        if let RateLimitDecision::Limited { retry_after, scope } =
            self.app.tools().rate_limiter().check(&rl_key)
        {
            let retry_secs = retry_after.as_secs_f64().ceil().max(1.0) as u64;
            return (
                429,
                status_reason(429),
                "application/json".to_string(),
                Some(vec![("Retry-After".to_string(), retry_secs.to_string())]),
                json!({
                    "ok": false,
                    "error": "rate_limited",
                    "scope": scope.as_str(),
                    "retryAfterMs": retry_after.as_millis() as u64,
                })
                .to_string()
                .into_bytes(),
            );
        }

        let seg = match self.resolve_identity(&req.path) {
            Some(s) => s,
            None => {
                return (
                    404,
                    "Not Found".to_string(),
                    "application/json".to_string(),
                    None,
                    json!({
                        "ok": false,
                        "error": "path must be /{creatureId}/{programId}/{entityId}/{vmId}/{path…} or /{creatureUsername}/{customPath…}"
                    })
                    .to_string()
                    .into_bytes(),
                );
            }
        };

        let mut headers_map = serde_json::Map::new();
        for (k, v) in &req.headers {
            headers_map.insert(k.clone(), json!(v));
        }

        // Package the request and hand it to this node instance's VMM to
        // forward — the ingress is a pure HTTP adapter and never reaches into
        // the packet router or plugin registry itself.
        let request = json!({
            "creatureId": seg.creature_id,
            "programId": seg.program_id,
            "entityId": seg.entity_id,
            "vmId": seg.vm_id,
            "runtime": seg.runtime,
            "method": req.method,
            "path": seg.rest_path,
            "query": req.query,
            "headers": JsonValue::Object(headers_map),
            "bodyBase64": BASE64_STANDARD.encode(&req.body),
        });

        let value = self.app.tools().workloads().forward_http(&request);

        if value["ok"].as_bool() != Some(true) {
            let err = value["error"]
                .as_str()
                .unwrap_or("forwarding failed")
                .to_string();
            let status = value["status"].as_u64().unwrap_or(502) as u16;
            return (
                status,
                status_reason(status),
                "application/json".to_string(),
                None,
                json!({"ok": false, "error": err}).to_string().into_bytes(),
            );
        }

        let status = value["status"].as_u64().unwrap_or(200) as u16;
        let (content_type, extra_headers) = extract_headers(&value);
        let body = decode_body(&value);
        (
            status,
            status_reason(status),
            content_type,
            extra_headers,
            body,
        )
    }

    /// Resolve a request path to the VM identity it targets. A deployer-defined
    /// custom route (`/{creatureUsername}/{customPath…}`) is tried first: the
    /// leading segment is looked up as a creature username and matched against
    /// the routes registered for that creature at deploy time. When no custom
    /// route matches, the fully-qualified identity form
    /// (`/{creatureId}/{programId}/{entityId}/{vmId}/{path…}`) is parsed
    /// directly, so the two forms coexist without ambiguity — a legacy request's
    /// leading segment is a creature *id*, which is never a username.
    fn resolve_identity(&self, path: &str) -> Option<IdentitySegments> {
        if let Some((first, rest)) = split_first_segment(path) {
            if let Some(route) = self.app.tools().workloads().resolve_http_route(first, rest) {
                let program_id = route["programId"].as_str().unwrap_or("").to_string();
                let entity_id = route["entityId"].as_str().unwrap_or("").to_string();
                if !program_id.is_empty() && !entity_id.is_empty() {
                    return Some(IdentitySegments {
                        creature_id: route["creatureId"].as_str().unwrap_or("").to_string(),
                        program_id,
                        entity_id,
                        vm_id: route["vmId"].as_str().unwrap_or("").to_string(),
                        runtime: route["runtime"].as_str().unwrap_or("").to_string(),
                        rest_path: route["path"].as_str().unwrap_or("/").to_string(),
                    });
                }
            }
        }
        split_identity(path)
    }
}

/// Split a request path into its leading segment and the remainder
/// (`"/alice@global/api/x"` → `("alice@global", "api/x")`). Used to attempt
/// custom-path routing before the fully-qualified identity form.
fn split_first_segment(path: &str) -> Option<(&str, &str)> {
    let trimmed = path.trim_start_matches('/');
    let mut it = trimmed.splitn(2, '/');
    let first = it.next().filter(|s| !s.is_empty())?;
    let rest = it.next().unwrap_or("");
    Some((first, rest))
}

struct HttpRequest {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct IdentitySegments {
    creature_id: String,
    program_id: String,
    entity_id: String,
    vm_id: String,
    /// Target runtime captured on the custom route (empty for the identity form,
    /// where the VMM derives it from program/entity state).
    runtime: String,
    rest_path: String,
}

/// Split a request path into
/// `/{creatureId}/{programId}/{entityId}/{vmId}/{rest…}`. All four identity
/// segments are required; the VM id names the specific instance the request is
/// forwarded to.
fn split_identity(path: &str) -> Option<IdentitySegments> {
    let trimmed = path.trim_start_matches('/');
    let mut it = trimmed.splitn(5, '/');
    let creature_id = it.next().filter(|s| !s.is_empty())?.to_string();
    let program_id = it.next().filter(|s| !s.is_empty())?.to_string();
    let entity_id = it.next().filter(|s| !s.is_empty())?.to_string();
    let vm_id = it.next().filter(|s| !s.is_empty())?.to_string();
    let rest = it.next().unwrap_or("");
    let rest_path = format!("/{}", rest);
    Some(IdentitySegments {
        creature_id,
        program_id,
        entity_id,
        vm_id,
        runtime: String::new(),
        rest_path,
    })
}

/// Parse a minimal HTTP/1.1 request off `stream`: request line, headers, and a
/// Content-Length-bounded body.
fn parse_request(stream: &mut TcpStream) -> Result<HttpRequest, String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);

    let mut request_line = String::new();
    reader
        .read_line(&mut request_line)
        .map_err(|e| format!("read request line: {}", e))?;
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Err("malformed request line".to_string());
    }
    let method = parts[0].to_string();
    let target = parts[1].to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };

    let mut headers: Vec<(String, String)> = Vec::new();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        let n = reader
            .read_line(&mut line)
            .map_err(|e| format!("read header: {}", e))?;
        if n == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let key = k.trim().to_string();
            let val = v.trim().to_string();
            if key.eq_ignore_ascii_case("content-length") {
                content_length = val.parse::<usize>().unwrap_or(0).min(MAX_BODY);
            }
            headers.push((key, val));
        }
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader
            .read_exact(&mut body)
            .map_err(|e| format!("read body: {}", e))?;
    }

    Ok(HttpRequest {
        method,
        path,
        query,
        headers,
        body,
    })
}

/// Read `content-type` and any other headers a plugin returned in its response.
fn extract_headers(value: &JsonValue) -> (String, Option<Vec<(String, String)>>) {
    let mut content_type = "application/octet-stream".to_string();
    let mut extra: Vec<(String, String)> = Vec::new();
    if let Some(obj) = value["headers"].as_object() {
        for (k, v) in obj {
            let val = match v.as_str() {
                Some(s) => s.to_string(),
                None => v.to_string(),
            };
            if k.eq_ignore_ascii_case("content-type") {
                content_type = val;
            } else if k.eq_ignore_ascii_case("content-length")
                || k.eq_ignore_ascii_case("transfer-encoding")
                || k.eq_ignore_ascii_case("connection")
            {
                // Recomputed by the ingress; never pass through.
                continue;
            } else {
                extra.push((k.clone(), val));
            }
        }
    }
    let extra = if extra.is_empty() { None } else { Some(extra) };
    (content_type, extra)
}

/// Decode a plugin response body from `bodyBase64` (preferred) or `body`.
fn decode_body(value: &JsonValue) -> Vec<u8> {
    if let Some(b64) = value["bodyBase64"].as_str() {
        if let Ok(bytes) = BASE64_STANDARD.decode(b64) {
            return bytes;
        }
    }
    match value["body"].as_str() {
        Some(s) => s.as_bytes().to_vec(),
        None if value["body"].is_null() => Vec::new(),
        None => value["body"].to_string().into_bytes(),
    }
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    extra_headers: Option<Vec<(String, String)>>,
    body: Vec<u8>,
) {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        status,
        reason,
        content_type,
        body.len()
    );
    if let Some(hs) = extra_headers {
        for (k, v) in hs {
            // Drop header values that would break the framing.
            if v.contains('\r') || v.contains('\n') {
                continue;
            }
            head.push_str(&format!("{}: {}\r\n", k, v));
        }
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

fn status_reason(status: u16) -> String {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "OK",
    }
    .to_string()
}
