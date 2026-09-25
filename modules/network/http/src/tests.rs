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
