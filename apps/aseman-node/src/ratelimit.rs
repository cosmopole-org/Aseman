//! Token-bucket rate limiter — the concrete [`IRateLimiter`] the node uses to
//! throttle client → node requests across every transport.
//!
//! # Why a token bucket
//!
//! A token bucket is the standard, production-grade choice for request
//! admission control: it enforces a *sustained* rate (`rate_per_sec`, the
//! steady-state refill) while still permitting short *bursts* up to a
//! configurable ceiling (`burst`, the bucket capacity). That matches real
//! client behaviour — apps fire a flurry of requests on a screen open, then go
//! quiet — far better than a fixed window (which allows 2× the rate across a
//! window boundary) or a leaky bucket (which forbids bursts entirely).
//!
//! # Cross-protocol unification
//!
//! One `RateLimiter` instance is shared by the TCP, WebSocket, and HTTP-ingress
//! transports (it is owned by the node and
//! owned by the node). The bucket key is derived from the
//! caller's *identity*, never the wire it came in on, so a client draws from a
//! single quota no matter how it distributes load across protocols.
//!
//! # Tiers and scopes
//!
//! * **Authenticated tier** — keyed by verified `user_id`, the natural fairness
//!   unit for a logged-in client. Gets the more generous limit.
//! * **Anonymous tier** — keyed by peer IP, applied to pre-auth traffic
//!   (handshakes, `authenticate`, unauthenticated HTTP). Tighter, to blunt
//!   credential-stuffing and connect-flood abuse. Keying on the (hard to spoof
//!   on an established TLS connection) peer IP is what makes this evasion
//!   resistant.
//! * **Global scope** — an optional node-wide aggregate limiter that protects
//!   the process from a distributed flood no single identity would trip. It is
//!   a coarse safety net, checked after the per-identity bucket.
//!
//! # Memory
//!
//! Per-identity buckets live in a [`DashMap`]; a background sweeper evicts
//! buckets idle longer than the configured TTL so the map cannot grow without
//! bound under churny or adversarial key sets.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use dashmap::DashMap;

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
}

impl Protocol {
    /// Stable lowercase label used in logs and telemetry.
    pub fn as_str(self) -> &'static str {
        match self {
            Protocol::Tcp => "tcp",
            Protocol::Ws => "ws",
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
    /// Verified authenticated user id, or empty for anonymous traffic.
    pub user_id: String,
    /// Remote peer IP (best-effort; may be empty for in-process callers).
    pub peer_ip: String,
}

impl RateLimitKey {
    /// Build a key for a verified-authenticated request.
    pub fn authenticated(user_id: &str, peer_ip: &str) -> Self {
        RateLimitKey {
            user_id: user_id.to_string(),
            peer_ip: peer_ip.to_string(),
        }
    }

    /// Build a key for anonymous (pre-auth) traffic, billed to the peer IP.
    pub fn anonymous(peer_ip: &str) -> Self {
        RateLimitKey {
            user_id: String::new(),
            peer_ip: peer_ip.to_string(),
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
    /// The request may proceed.
    Allowed,
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
    #[cfg(test)]
    pub fn is_allowed(&self) -> bool {
        matches!(self, RateLimitDecision::Allowed)
    }
}

/// A point-in-time snapshot of limiter counters for telemetry.
#[derive(Debug, Clone, Default)]
#[cfg(test)]
pub struct RateLimiterSnapshot {
    /// Whether enforcement is currently on.
    pub enabled: bool,
    /// Number of live per-identity buckets currently tracked.
    pub tracked_identities: u64,
    /// Total requests admitted since start.
    pub allowed: u64,
    /// Total requests rejected by a per-identity bucket since start.
    pub limited_identity: u64,
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

/// Static limits for one tier (authenticated or anonymous).
#[derive(Debug, Clone, Copy)]
pub struct TierConfig {
    /// Sustained refill rate, in tokens (requests) per second.
    pub rate_per_sec: f64,
    /// Bucket capacity — the largest instantaneous burst allowed.
    pub burst: f64,
}

impl TierConfig {
    fn sanitized(self) -> TierConfig {
        TierConfig {
            rate_per_sec: if self.rate_per_sec.is_finite() && self.rate_per_sec > 0.0 {
                self.rate_per_sec
            } else {
                f64::MIN_POSITIVE
            },
            // A bucket must hold at least one whole token or nothing would ever
            // pass; clamp up to 1.0.
            burst: if self.burst.is_finite() && self.burst >= 1.0 {
                self.burst
            } else {
                1.0
            },
        }
    }
}

/// Full limiter configuration, built from the composition root's typed values.
#[derive(Debug, Clone, Copy)]
pub struct RateLimiterConfig {
    /// Master switch. When `false` every request is admitted.
    pub enabled: bool,
    /// Limits for verified-authenticated identities.
    pub authenticated: TierConfig,
    /// Limits for anonymous / pre-auth traffic (keyed by IP).
    pub anonymous: TierConfig,
    /// Node-wide aggregate limit. `rate_per_sec <= 0` disables the global gate.
    pub global: TierConfig,
    /// Whether the node-wide global gate is active.
    pub global_enabled: bool,
    /// Evict per-identity buckets untouched for at least this long.
    pub idle_ttl: Duration,
}

impl Default for RateLimiterConfig {
    fn default() -> Self {
        RateLimiterConfig {
            enabled: true,
            // ~50 req/s sustained, 100 burst for a signed-in client.
            authenticated: TierConfig {
                rate_per_sec: 50.0,
                burst: 100.0,
            },
            // Tighter for anonymous callers sharing an IP bucket.
            anonymous: TierConfig {
                rate_per_sec: 10.0,
                burst: 20.0,
            },
            // Node-wide safety net.
            global: TierConfig {
                rate_per_sec: 5000.0,
                burst: 10000.0,
            },
            global_enabled: true,
            idle_ttl: Duration::from_secs(300),
        }
    }
}

impl RateLimiterConfig {
    pub fn from_typed(source: &aseman_config::RateLimitConfig) -> RateLimiterConfig {
        let idle_seconds =
            if source.idle_evict_seconds.is_finite() && source.idle_evict_seconds > 0.0 {
                source.idle_evict_seconds
            } else {
                300.0
            };
        let mut cfg = RateLimiterConfig {
            enabled: source.enabled,
            authenticated: TierConfig {
                rate_per_sec: source.authenticated_rps,
                burst: source.authenticated_burst,
            },
            anonymous: TierConfig {
                rate_per_sec: source.anonymous_rps,
                burst: source.anonymous_burst,
            },
            global: TierConfig {
                rate_per_sec: source.global_rps,
                burst: source.global_burst,
            },
            global_enabled: source.global_rps > 0.0,
            idle_ttl: Duration::from_secs_f64(idle_seconds),
        };
        cfg.authenticated = cfg.authenticated.sanitized();
        cfg.anonymous = cfg.anonymous.sanitized();
        cfg.global = cfg.global.sanitized();
        cfg
    }
}

/// A single token bucket. All time is passed in explicitly so the refill logic
/// is deterministic and unit-testable.
#[derive(Debug)]
struct Bucket {
    tokens: f64,
    capacity: f64,
    rate_per_sec: f64,
    last_refill: Instant,
    last_seen: Instant,
}

impl Bucket {
    fn new(cfg: TierConfig, now: Instant) -> Bucket {
        Bucket {
            // Start full so a fresh client gets its whole burst immediately.
            tokens: cfg.burst,
            capacity: cfg.burst,
            rate_per_sec: cfg.rate_per_sec,
            last_refill: now,
            last_seen: now,
        }
    }

    /// Refill according to elapsed wall-clock time, then try to spend one
    /// token. Returns `Ok(remaining_whole_tokens)` on success, or
    /// `Err(retry_after)` with the wait until one token is available.
    fn try_acquire(&mut self, now: Instant) -> Result<u32, Duration> {
        // Refill. `saturating_duration_since` guards against any non-monotonic
        // clock surprises (returns zero rather than panicking).
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * self.rate_per_sec).min(self.capacity);
            self.last_refill = now;
        }
        self.last_seen = now;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            Ok(self.tokens.floor() as u32)
        } else {
            let deficit = 1.0 - self.tokens;
            let secs = deficit / self.rate_per_sec;
            Err(Duration::from_secs_f64(secs.min(3600.0)))
        }
    }
}

/// The node's token-bucket [`IRateLimiter`].
pub struct RateLimiter {
    cfg: RateLimiterConfig,
    /// Per-identity buckets, keyed by `"u:{user_id}"` / `"ip:{peer_ip}"`.
    buckets: Arc<DashMap<String, Bucket>>,
    /// Node-wide aggregate bucket (only consulted when `global_enabled`).
    global: Arc<std::sync::Mutex<Bucket>>,

    allowed: AtomicU64,
    limited_identity: AtomicU64,
}

impl RateLimiter {
    /// Build a limiter from explicit config and start the idle-bucket sweeper.
    pub fn new(cfg: RateLimiterConfig) -> Arc<RateLimiter> {
        let now = Instant::now();
        let limiter = Arc::new(RateLimiter {
            cfg,
            buckets: Arc::new(DashMap::new()),
            global: Arc::new(std::sync::Mutex::new(Bucket::new(cfg.global, now))),
            allowed: AtomicU64::new(0),
            limited_identity: AtomicU64::new(0),
        });
        if cfg.enabled {
            limiter.clone().spawn_sweeper();
        }
        limiter
    }

    /// Build a limiter from validated typed configuration.
    pub fn from_typed(source: &aseman_config::RateLimitConfig) -> Arc<RateLimiter> {
        let cfg = RateLimiterConfig::from_typed(source);
        eprintln!(
            "[ratelimit] enabled={} auth={}/{} anon={}/{} global={} idle_ttl={}s",
            cfg.enabled,
            cfg.authenticated.rate_per_sec,
            cfg.authenticated.burst,
            cfg.anonymous.rate_per_sec,
            cfg.anonymous.burst,
            if cfg.global_enabled {
                format!("{}/{}", cfg.global.rate_per_sec, cfg.global.burst)
            } else {
                "off".to_string()
            },
            cfg.idle_ttl.as_secs(),
        );
        RateLimiter::new(cfg)
    }

    /// The bucket key + tier for a request key. Authenticated requests bill the
    /// verified user; everyone else bills their peer IP. A missing peer IP
    /// (in-process callers) collapses to a single shared anonymous bucket,
    /// which is the safe, conservative default.
    fn resolve(&self, key: &RateLimitKey) -> (String, TierConfig) {
        if key.is_authenticated() {
            (format!("u:{}", key.user_id), self.cfg.authenticated)
        } else {
            let ip = if key.peer_ip.is_empty() {
                "unknown"
            } else {
                key.peer_ip.as_str()
            };
            (format!("ip:{}", ip), self.cfg.anonymous)
        }
    }

    fn check_at(&self, key: &RateLimitKey, now: Instant) -> RateLimitDecision {
        if !self.cfg.enabled {
            return RateLimitDecision::Allowed;
        }

        let (bucket_key, tier) = self.resolve(key);

        // 1) Per-identity bucket. Consulted first so a well-behaved client's
        //    global tokens are not spent on a request its own bucket rejects.
        let identity_result = {
            let mut entry = self
                .buckets
                .entry(bucket_key)
                .or_insert_with(|| Bucket::new(tier, now));
            entry.try_acquire(now)
        };
        let _remaining = match identity_result {
            Ok(remaining) => remaining,
            Err(retry_after) => {
                self.limited_identity.fetch_add(1, Ordering::Relaxed);
                return RateLimitDecision::Limited {
                    retry_after,
                    scope: LimitScope::Identity,
                };
            }
        };

        // 2) Node-wide safety net. A coarse aggregate cap that no single
        //    identity would reach on its own.
        if self.cfg.global_enabled {
            let mut g = self.global.lock().unwrap();
            if let Err(retry_after) = g.try_acquire(now) {
                return RateLimitDecision::Limited {
                    retry_after,
                    scope: LimitScope::Global,
                };
            }
        }

        self.allowed.fetch_add(1, Ordering::Relaxed);
        RateLimitDecision::Allowed
    }

    /// Drop buckets idle for longer than the configured TTL.
    fn evict_idle(&self, now: Instant) {
        let ttl = self.cfg.idle_ttl;
        self.buckets
            .retain(|_, b| now.saturating_duration_since(b.last_seen) < ttl);
    }

    fn spawn_sweeper(self: Arc<Self>) {
        // Sweep at a fraction of the TTL (bounded to a sane range) so evictions
        // are timely without busy-looping.
        let interval = self
            .cfg
            .idle_ttl
            .max(Duration::from_secs(10))
            .min(Duration::from_secs(300));
        let weak = Arc::downgrade(&self);
        thread::Builder::new()
            .name("ratelimit-sweeper".to_string())
            .spawn(move || {
                loop {
                    thread::sleep(interval);
                    // Stop once the limiter itself is dropped.
                    match weak.upgrade() {
                        Some(limiter) => limiter.evict_idle(Instant::now()),
                        None => break,
                    }
                }
            })
            .ok();
    }
}

impl RateLimiter {
    pub(crate) fn check(&self, key: &RateLimitKey) -> RateLimitDecision {
        self.check_at(key, Instant::now())
    }
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> RateLimiterSnapshot {
        RateLimiterSnapshot {
            enabled: self.cfg.enabled,
            tracked_identities: self.buckets.len() as u64,
            allowed: self.allowed.load(Ordering::Relaxed),
            limited_identity: self.limited_identity.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier(rate: f64, burst: f64) -> TierConfig {
        TierConfig {
            rate_per_sec: rate,
            burst,
        }
    }

    #[test]
    fn bucket_allows_up_to_burst_then_blocks() {
        let t0 = Instant::now();
        let mut b = Bucket::new(tier(1.0, 3.0), t0);
        // Burst of 3 succeeds back-to-back with no time passing.
        assert!(b.try_acquire(t0).is_ok());
        assert!(b.try_acquire(t0).is_ok());
        assert!(b.try_acquire(t0).is_ok());
        // Fourth immediate request is refused.
        let err = b.try_acquire(t0).unwrap_err();
        assert!(err > Duration::ZERO);
    }

    #[test]
    fn bucket_refills_over_time() {
        let t0 = Instant::now();
        let mut b = Bucket::new(tier(2.0, 2.0), t0); // 2 tokens/sec
        assert!(b.try_acquire(t0).is_ok());
        assert!(b.try_acquire(t0).is_ok());
        assert!(b.try_acquire(t0).is_err());
        // After 1 second, 2 tokens have refilled.
        let t1 = t0 + Duration::from_secs(1);
        assert!(b.try_acquire(t1).is_ok());
        assert!(b.try_acquire(t1).is_ok());
        assert!(b.try_acquire(t1).is_err());
    }

    #[test]
    fn bucket_never_exceeds_capacity() {
        let t0 = Instant::now();
        let mut b = Bucket::new(tier(100.0, 5.0), t0);
        // Idle a long time: refill must clamp at capacity (5), not 100*10.
        let t1 = t0 + Duration::from_secs(10);
        for _ in 0..5 {
            assert!(b.try_acquire(t1).is_ok());
        }
        assert!(b.try_acquire(t1).is_err());
    }

    #[test]
    fn retry_after_is_reasonable() {
        let t0 = Instant::now();
        let mut b = Bucket::new(tier(1.0, 1.0), t0); // 1 token/sec, capacity 1
        assert!(b.try_acquire(t0).is_ok());
        let retry = b.try_acquire(t0).unwrap_err();
        // Need one full token at 1/sec ≈ 1s.
        assert!(retry >= Duration::from_millis(900) && retry <= Duration::from_millis(1100));
    }

    #[test]
    fn disabled_limiter_always_allows() {
        let cfg = RateLimiterConfig {
            enabled: false,
            authenticated: tier(1.0, 1.0),
            ..Default::default()
        };
        let rl = RateLimiter::new(cfg);
        let key = RateLimitKey::authenticated("u1", "1.2.3.4");
        for _ in 0..1000 {
            assert!(rl.check(&key).is_allowed());
        }
    }

    #[test]
    fn authenticated_and_anonymous_use_separate_buckets() {
        let cfg = RateLimiterConfig {
            enabled: true,
            authenticated: tier(1.0, 2.0),
            anonymous: tier(1.0, 2.0),
            global: tier(1000.0, 1000.0),
            global_enabled: true,
            idle_ttl: Duration::from_secs(300),
        };
        let rl = RateLimiter::new(cfg);
        let now = Instant::now();
        let user = RateLimitKey::authenticated("u1", "1.2.3.4");
        let anon = RateLimitKey::anonymous("1.2.3.4");
        // Drain the user's bucket.
        assert!(rl.check_at(&user, now).is_allowed());
        assert!(rl.check_at(&user, now).is_allowed());
        assert!(!rl.check_at(&user, now).is_allowed());
        // The anonymous bucket for the same IP is independent and still full.
        assert!(rl.check_at(&anon, now).is_allowed());
        assert!(rl.check_at(&anon, now).is_allowed());
        assert!(!rl.check_at(&anon, now).is_allowed());
    }

    #[test]
    fn same_identity_is_shared_across_protocols() {
        let cfg = RateLimiterConfig {
            enabled: true,
            authenticated: tier(1.0, 2.0),
            anonymous: tier(1.0, 2.0),
            global: tier(1000.0, 1000.0),
            global_enabled: true,
            idle_ttl: Duration::from_secs(300),
        };
        let rl = RateLimiter::new(cfg);
        let now = Instant::now();
        // Two whole tokens of burst, spent one per protocol — the third
        // (regardless of protocol) is refused because the quota is unified.
        let tcp = RateLimitKey::authenticated("u1", "1.2.3.4");
        let ws = RateLimitKey::authenticated("u1", "1.2.3.4");
        let http = RateLimitKey::authenticated("u1", "1.2.3.4");
        assert!(rl.check_at(&tcp, now).is_allowed());
        assert!(rl.check_at(&ws, now).is_allowed());
        assert!(!rl.check_at(&http, now).is_allowed());
    }

    #[test]
    fn global_limiter_trips_across_distinct_identities() {
        let cfg = RateLimiterConfig {
            enabled: true,
            authenticated: tier(1000.0, 1000.0),
            anonymous: tier(1000.0, 1000.0),
            // Global cap of 2 total, no refill within the test window.
            global: tier(0.001, 2.0),
            global_enabled: true,
            idle_ttl: Duration::from_secs(300),
        };
        let rl = RateLimiter::new(cfg);
        let now = Instant::now();
        let a = RateLimitKey::authenticated("a", "1.1.1.1");
        let b = RateLimitKey::authenticated("b", "2.2.2.2");
        let c = RateLimitKey::authenticated("c", "3.3.3.3");
        assert!(rl.check_at(&a, now).is_allowed());
        assert!(rl.check_at(&b, now).is_allowed());
        // Third distinct identity is fine per-identity but trips the global cap.
        match rl.check_at(&c, now) {
            RateLimitDecision::Limited { scope, .. } => assert_eq!(scope, LimitScope::Global),
            other => panic!("expected global limit, got {:?}", other),
        }
    }

    #[test]
    fn idle_buckets_are_evicted() {
        let cfg = RateLimiterConfig {
            enabled: true,
            authenticated: tier(1.0, 2.0),
            anonymous: tier(1.0, 2.0),
            global: tier(1000.0, 1000.0),
            global_enabled: true,
            idle_ttl: Duration::from_secs(60),
        };
        let rl = RateLimiter::new(cfg);
        let now = Instant::now();
        let key = RateLimitKey::authenticated("u1", "1.2.3.4");
        assert!(rl.check_at(&key, now).is_allowed());
        assert_eq!(rl.buckets.len(), 1);
        // Not yet past the TTL.
        rl.evict_idle(now + Duration::from_secs(30));
        assert_eq!(rl.buckets.len(), 1);
        // Past the TTL — the bucket is swept.
        rl.evict_idle(now + Duration::from_secs(61));
        assert_eq!(rl.buckets.len(), 0);
    }

    #[test]
    fn snapshot_tracks_counters() {
        let cfg = RateLimiterConfig {
            enabled: true,
            authenticated: tier(1.0, 1.0),
            anonymous: tier(1.0, 1.0),
            global: tier(1000.0, 1000.0),
            global_enabled: true,
            idle_ttl: Duration::from_secs(300),
        };
        let rl = RateLimiter::new(cfg);
        let now = Instant::now();
        let key = RateLimitKey::authenticated("u1", "1.2.3.4");
        assert!(rl.check_at(&key, now).is_allowed());
        assert!(!rl.check_at(&key, now).is_allowed());
        let snap = rl.snapshot();
        assert!(snap.enabled);
        assert_eq!(snap.allowed, 1);
        assert_eq!(snap.limited_identity, 1);
        assert_eq!(snap.tracked_identities, 1);
    }

    #[test]
    fn config_sanitizes_pathological_values() {
        let bad = TierConfig {
            rate_per_sec: -5.0,
            burst: 0.0,
        }
        .sanitized();
        assert!(bad.rate_per_sec > 0.0);
        assert!(bad.burst >= 1.0);
    }
}
