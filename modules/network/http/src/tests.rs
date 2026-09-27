use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use super::*;

#[derive(Default)]
struct RecordingService {
    requests: Mutex<Vec<PublicActionRequest>>,
}

struct StaticSubscription {
    batch: Mutex<Option<PublicEventBatch>>,
}

impl PublicEventSubscription for StaticSubscription {
    fn read(
        &self,
        _after_sequence: u64,
        _limit: usize,
    ) -> Result<PublicEventBatch, PublicActionError> {
        Ok(self
            .batch
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| PublicEventBatch::Events(Vec::new())))
    }
}

struct RecordingEventService {
    subscriptions: Mutex<Vec<(String, String)>>,
    batch: PublicEventBatch,
}

impl PublicEventService for RecordingEventService {
    fn subscribe(
        &self,
        request: PublicEventRequest,
    ) -> Result<Arc<dyn PublicEventSubscription>, PublicActionError> {
        self.subscriptions
            .lock()
            .unwrap()
            .push((request.stream, request.token));
        Ok(Arc::new(StaticSubscription {
            batch: Mutex::new(Some(self.batch.clone())),
        }))
    }
}

impl PublicActionService for RecordingService {
    fn invoke(
        &self,
        request: PublicActionRequest,
    ) -> Result<PublicActionResponse, PublicActionError> {
        self.requests.lock().unwrap().push(request);
        Ok(PublicActionResponse {
            status: 200,
            body: br#"{"ok":true}"#.to_vec(),
        })
    }
}

fn config() -> PublicHttpConfig {
    PublicHttpConfig {
        max_body_bytes: 128,
        max_in_flight: 2,
        request_timeout: Duration::from_secs(1),
        requests_per_window: 100,
        rate_window: Duration::from_secs(60),
        max_rate_subjects: 100,
        allowed_origins: ["https://console.example".to_owned()].into_iter().collect(),
        drain_timeout: Duration::from_secs(1),
    }
}

fn request(path: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(SESSION_HEADER, "session-token")
        .header(REQUEST_ID_HEADER, "request-123")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap()
}

async fn problem_reason(response: Response) -> String {
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice::<Value>(&body).unwrap()["aseman_reason"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn catalog_is_the_generated_contract_and_withholds_login() {
    let catalog = RouteCatalog::default();
    assert_eq!(catalog.0.len(), 76);
    assert!(catalog.operation("/v1/actions/api/hello").is_some());
    assert!(catalog.operation("/v1/actions/creatures/login").is_none());
}

#[tokio::test]
async fn read_action_reaches_only_the_service_boundary() {
    let service = Arc::new(RecordingService::default());
    let response = router(service.clone(), config())
        .oneshot(request("/v1/actions/api/hello"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["aseman-request-id"], "request-123");
    assert_eq!(response.headers()["cache-control"], "no-store");
    let recorded = service.requests.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].route, "/v1/actions/api/hello");
    assert_eq!(recorded[0].action, "node.diagnostics.read");
    assert_eq!(recorded[0].class, ActionClass::Read);
    assert!(recorded[0].idempotency_key.is_none());
}

#[tokio::test]
async fn mutation_requires_a_well_formed_idempotency_key() {
    let service = Arc::new(RecordingService::default());
    let response = router(service.clone(), config())
        .oneshot(request("/v1/actions/creatures/create"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(problem_reason(response).await, "idempotency_key_required");
    assert!(service.requests.lock().unwrap().is_empty());

    let mut valid = request("/v1/actions/creatures/create");
    valid.headers_mut().insert(
        HeaderName::from_static("idempotency-key"),
        HeaderValue::from_static("request-key-0001"),
    );
    let response = router(service.clone(), config())
        .oneshot(valid)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        service.requests.lock().unwrap()[0]
            .idempotency_key
            .as_deref(),
        Some("request-key-0001")
    );
}

#[tokio::test]
async fn rejects_unknown_withheld_ambiguous_and_oversized_requests_before_service() {
    let service = Arc::new(RecordingService::default());
    for path in ["/v1/actions/not/registered", "/v1/actions/creatures/login"] {
        let response = router(service.clone(), config())
            .oneshot(request(path))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    let mut ambiguous = request("/v1/actions/api/hello");
    ambiguous
        .headers_mut()
        .insert(PROOF_HEADER, HeaderValue::from_static("not-a-proof"));
    let response = router(service.clone(), config())
        .oneshot(ambiguous)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(problem_reason(response).await, "ambiguous_authentication");

    let oversized = Request::builder()
        .method("POST")
        .uri("/v1/actions/api/hello")
        .header(SESSION_HEADER, "session-token")
        .body(Body::from("x".repeat(129)))
        .unwrap();
    let response = router(service.clone(), config())
        .oneshot(oversized)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(service.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn cors_is_fail_closed_and_preflight_is_bounded_to_known_routes() {
    let service = Arc::new(RecordingService::default());
    let mut refused = request("/v1/actions/api/hello");
    refused.headers_mut().insert(
        header::ORIGIN,
        HeaderValue::from_static("https://attacker.example"),
    );
    let response = router(service.clone(), config())
        .oneshot(refused)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let preflight = Request::builder()
        .method(Method::OPTIONS)
        .uri("/v1/actions/api/hello")
        .header(header::ORIGIN, "https://console.example")
        .body(Body::empty())
        .unwrap();
    let response = router(service.clone(), config())
        .oneshot(preflight)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
        "https://console.example"
    );
    assert!(service.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invalid_json_never_reaches_application_code() {
    let service = Arc::new(RecordingService::default());
    let invalid = Request::builder()
        .method("POST")
        .uri("/v1/actions/api/hello")
        .header(SESSION_HEADER, "session-token")
        .body(Body::from("[]"))
        .unwrap();
    let response = router(service.clone(), config())
        .oneshot(invalid)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(problem_reason(response).await, "invalid_request");
    assert!(service.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn sse_replays_bounded_authorized_events_with_sequence_ids() {
    let actions = Arc::new(RecordingService::default());
    let events = Arc::new(RecordingEventService {
        subscriptions: Mutex::new(Vec::new()),
        batch: PublicEventBatch::Events(vec![
            PublicEventFrame {
                event_id: "event-5".to_owned(),
                sequence: 5,
                kind: "store.updated".to_owned(),
                data: r#"{"event":{"id":"event-5"}}"#.to_owned(),
            },
            PublicEventFrame {
                event_id: "event-6".to_owned(),
                sequence: 6,
                kind: "store.updated".to_owned(),
                data: r#"{"event":{"id":"event-6"}}"#.to_owned(),
            },
        ]),
    });
    let request = Request::builder()
        .method(Method::GET)
        .uri("/v1/events/creature:one?maxEvents=2")
        .header(SESSION_HEADER, "session-token")
        .header(BRIDGE_TOKEN_HEADER, "bridge-token")
        .header("last-event-id", "4")
        .body(Body::empty())
        .unwrap();
    let response = router_with_events(actions, Some(events.clone()), config())
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("id: 5"));
    assert!(body.contains("id: 6"));
    assert!(body.contains("event: store.updated"));
    assert_eq!(
        events.subscriptions.lock().unwrap().as_slice(),
        &[("creature:one".to_owned(), "bridge-token".to_owned())]
    );
}

#[tokio::test]
async fn sse_emits_resync_and_refuses_unauthenticated_admission() {
    let actions = Arc::new(RecordingService::default());
    let events = Arc::new(RecordingEventService {
        subscriptions: Mutex::new(Vec::new()),
        batch: PublicEventBatch::Resync {
            oldest_sequence: Some(20),
            latest_sequence: Some(30),
        },
    });
    let admitted = Request::builder()
        .method(Method::GET)
        .uri("/v1/events/creature:one?after=2&maxEvents=1")
        .header(SESSION_HEADER, "session-token")
        .header(BRIDGE_TOKEN_HEADER, "bridge-token")
        .body(Body::empty())
        .unwrap();
    let response = router_with_events(actions.clone(), Some(events.clone()), config())
        .oneshot(admitted)
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(body.contains("event: resync"));
    assert!(body.contains(r#""oldestSequence":20"#));

    let unauthenticated = Request::builder()
        .method(Method::GET)
        .uri("/v1/events/creature:one?maxEvents=1")
        .header(BRIDGE_TOKEN_HEADER, "bridge-token")
        .body(Body::empty())
        .unwrap();
    let response = router_with_events(actions, Some(events.clone()), config())
        .oneshot(unauthenticated)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(events.subscriptions.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn terminal_route_refuses_requests_without_a_live_http_upgrade() {
    let request = Request::builder()
        .method(Method::GET)
        .uri("/v1/terminals/0190f1a2-7b3c-7d4e-8f00-112233445566?creatureId=p&vmId=v")
        .header(SESSION_HEADER, "session-token")
        .header("idempotency-key", "terminal-key-0001")
        .header(header::CONNECTION, "upgrade")
        .header(header::UPGRADE, "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .header("sec-websocket-protocol", "aseman.terminal.v1")
        .body(Body::empty())
        .unwrap();
    let response = router(Arc::new(RecordingService::default()), config())
        .oneshot(request)
        .await
        .unwrap();
    // Router-only requests have no hyper upgrade handle. Axum refuses them before
    // terminal admission; a real connection receives the provider-specific result.
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
}
