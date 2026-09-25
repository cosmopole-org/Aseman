//! The rate-limiting port — a protocol-agnostic admission control gate for
//! client → node requests.
//!
//! Every client-facing transport (TCP, WebSocket, and the VMM HTTP ingress)
//! funnels each inbound request through the same [`IRateLimiter`] before it is
//! dispatched into the action pipeline. Because all three transports share a
//! single limiter instance (hung off [`ITools`](crate::models::ports::tools),
//! reachable from every driver via `ICore`), a client cannot multiply its
//! allowance by spreading load across protocols: one identity draws from one
//! bucket regardless of the wire it arrived on. That cross-protocol unification
//! is the whole point of putting the contract here in the ports layer rather
//! than inside any single transport.
//!
//! The concrete implementation lives in
//! [`crate::drivers::ratelimit`].

use std::time::Duration;

/// The client-facing transport a request arrived on.
///
/// The protocol never partitions the quota — an identity's tokens are shared
/// across every transport — but it is carried through the check so the limiter
/// can attribute rejections per-protocol for telemetry and logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// The length-prefixed TLS-TCP client transport.
    Tcp,
    /// The TLS WebSocket client transport.
    Ws,
    /// The VMM HTTP ingress that forwards requests to VM instances.
    Http,
}

impl Protocol {
    /// Stable lowercase label used in logs and telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Tcp => "tcp",
            Protocol::Ws => "ws",
            Protocol::Http => "http",
        }
    }
}

/// Identifies who is making a request, so the limiter can pick the right
/// bucket and tier.
///
/// The limiter derives the bucket key from **verified** identity only:
///
/// * `user_id` must be an authenticated user (e.g. the id a transport has
///   pinned onto its socket *after* a successful signature check), never a
///   value merely claimed in an unverified packet. When it is non-empty the
///   request is billed to that user under the authenticated tier.
/// * otherwise the request is anonymous and billed to `peer_ip` under the
///   (tighter) anonymous tier.
///
/// Keeping spoofable, unverified identifiers out of the key is what makes the
/// limiter evasion-resistant: an attacker cannot mint fresh buckets by
/// rotating a forged user id, because a forged id is never trusted here.
#[derive(Debug, Clone)]
pub struct RateLimitKey {
    /// Transport the request arrived on (telemetry only).
    pub protocol: Protocol,
    /// Verified authenticated user id, or empty for anonymous traffic.
    pub user_id: String,
    /// Remote peer IP (best-effort; may be empty for in-process callers).
    pub peer_ip: String,
    /// Action path / request target being invoked (telemetry only).
    pub path: String,
}

impl RateLimitKey {
    /// Build a key for a verified-authenticated request.
    pub fn authenticated(protocol: Protocol, user_id: &str, peer_ip: &str, path: &str) -> Self {
        RateLimitKey {
            protocol,
            user_id: user_id.to_string(),
            peer_ip: peer_ip.to_string(),
            path: path.to_string(),
        }
    }

    /// Build a key for anonymous (pre-auth) traffic, billed to the peer IP.
    pub fn anonymous(protocol: Protocol, peer_ip: &str, path: &str) -> Self {
        RateLimitKey {
            protocol,
            user_id: String::new(),
            peer_ip: peer_ip.to_string(),
            path: path.to_string(),
        }
    }

    /// True when the request carries a verified authenticated identity.
    pub fn is_authenticated(&self) -> bool {
        !self.user_id.is_empty()
    }
}

/// Which limit rejected a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitScope {
    /// The per-identity (per-user or per-IP) bucket was exhausted.
    Identity,
    /// The node-wide aggregate limiter was exhausted.
    Global,
}

impl LimitScope {
    pub fn as_str(self) -> &'static str {
        match self {
            LimitScope::Identity => "identity",
            LimitScope::Global => "global",
        }
    }
}

/// The outcome of a single [`IRateLimiter::check`] call.
#[derive(Debug, Clone)]
pub enum RateLimitDecision {
    /// The request may proceed. `remaining` is the whole tokens left in the
    /// identity bucket after this request (best-effort, for `X-RateLimit-*`
    /// style headers).
    Allowed { remaining: u32 },
    /// The request is rejected. `retry_after` is the minimum wait before a
    /// retry could succeed against the exhausted bucket; `scope` says which
    /// limit tripped.
    Limited {
        retry_after: Duration,
        scope: LimitScope,
    },
}

impl RateLimitDecision {
    /// Convenience: whether the request was allowed through.
    pub fn is_allowed(&self) -> bool {
        matches!(self, RateLimitDecision::Allowed { .. })
    }
}

/// A point-in-time snapshot of limiter counters for telemetry.
#[derive(Debug, Clone, Default)]
pub struct RateLimiterSnapshot {
    /// Whether enforcement is currently on.
    pub enabled: bool,
    /// Number of live per-identity buckets currently tracked.
    pub tracked_identities: u64,
    /// Total requests admitted since start.
    pub allowed: u64,
    /// Total requests rejected by a per-identity bucket since start.
    pub limited_identity: u64,
    /// Total requests rejected by the global limiter since start.
    pub limited_global: u64,
}

/// Response code the length-prefixed client transports (TCP / WS) return when
/// a request is throttled. Chosen distinct from the existing action codes
/// (`0` ok, `1` not-found, `2` parse-error, `3` act-error, `4` auth-failed) so
/// clients can special-case a back-off without ambiguity. Mirrors HTTP `429`.
pub const RATE_LIMITED_RES_CODE: i64 = 8;

/// Build the JSON body handed back to a throttled client. Includes the machine
/// -readable `message`, the `retryAfterMs` a client should wait, and which
/// `scope` tripped. Old clients that only read `message` still see
/// `"rate_limited"`.
pub fn rate_limited_body(retry_after: Duration, scope: LimitScope) -> serde_json::Value {
    serde_json::json!({
        "message": "rate_limited",
        "retryAfterMs": retry_after.as_millis() as u64,
        "scope": scope.as_str(),
    })
}

/// Protocol-agnostic admission control for client → node requests.
pub trait IRateLimiter: Send + Sync {
    /// Consume one unit of quota for `key` and report whether the request may
    /// proceed. Implementations must be safe to call concurrently from many
    /// transport threads.
    fn check(&self, key: &RateLimitKey) -> RateLimitDecision;

    /// Whether enforcement is currently active. When `false`, [`check`](Self::check)
    /// always returns [`RateLimitDecision::Allowed`].
    fn enabled(&self) -> bool;

    /// Cheap snapshot of counters for telemetry / diagnostics.
    fn snapshot(&self) -> RateLimiterSnapshot;
}
