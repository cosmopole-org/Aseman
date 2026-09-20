//! Transport-neutral legacy client-session orchestration.
//!
//! TCP and WebSocket retain only framing and connection I/O. Authentication shortcuts,
//! rate-limit identity selection, action lookup/dispatch, response codes, listener
//! attachment, and gateway subscription effects have one owner here until the legacy
//! gateway is replaced by the canonical application gateway.

use std::sync::Arc;
use std::time::Instant;

use aseman_application::{classify_session_route, SessionRoute};
use serde_json::Value;

use crate::drivers::network::framing::decode_request_body;
use crate::models::core::ICore;
use crate::models::packet::{build_error_json, ResponseSimpleMessage};
use crate::models::ports::ratelimit::{
    rate_limited_body, Protocol, RateLimitDecision, RateLimitKey, RATE_LIMITED_RES_CODE,
};

pub(super) trait SessionSocket: Send + Sync {
    fn peer(&self) -> &str;
    fn peer_ip(&self) -> String;
    fn user_id(&self) -> String;
    fn listener_registered(&self) -> bool;
    fn mark_listener_registered(&self);
    fn write_response(&self, packet_id: &str, code: i64, body: &[u8]);
    fn write_update(&self, path: &str, body: &[u8]);
}

pub(super) trait SessionTransport<S: SessionSocket>: Send + Sync {
    fn app(&self) -> &Arc<dyn ICore>;
    fn protocol(&self) -> Protocol;
    fn attach_user_listener(&self, socket: &Arc<S>, user_id: &str);
    fn apply_gateway_subscription(&self, socket: &Arc<S>, path: &str, result: &Value);
}

pub(super) fn process_inbound<T, S>(transport: &T, socket: &Arc<S>, body: Vec<u8>)
where
    T: SessionTransport<S>,
    S: SessionSocket,
{
    let protocol = transport.protocol();
    let label = protocol.as_str();
    let body_len = body.len();
    let parsed = match decode_request_body(&body) {
        Ok(parsed) => parsed,
        Err(error) => {
            eprintln!(
                "[{label}] decode_request_body failed: peer={} body_len={body_len} err={error}",
                socket.peer()
            );
            return;
        }
    };
    let peer_ip = socket.peer_ip();
    let started = Instant::now();
    eprintln!(
        "[{label}] >> path={} user={} pkt={} payload_len={} peer={}",
        parsed.path,
        parsed.user_id,
        parsed.packet_id,
        parsed.payload.len(),
        socket.peer()
    );

    // Only a verified socket identity selects an authenticated bucket. A claimed packet
    // user never affects admission identity.
    let verified_user = socket.user_id();
    let rate_key = if verified_user.is_empty() {
        RateLimitKey::anonymous(protocol, &peer_ip, &parsed.path)
    } else {
        RateLimitKey::authenticated(protocol, &verified_user, &peer_ip, &parsed.path)
    };
    if let RateLimitDecision::Limited { retry_after, scope } =
        transport.app().tools().rate_limiter().check(&rate_key)
    {
        let response =
            serde_json::to_vec(&rate_limited_body(retry_after, scope)).unwrap_or_default();
        socket.write_response(&parsed.packet_id, RATE_LIMITED_RES_CODE, &response);
        eprintln!(
            "[{label}] << path={} pkt={} code={} rate_limited scope={} retry_ms={} elapsed_ms={}",
            parsed.path,
            parsed.packet_id,
            RATE_LIMITED_RES_CODE,
            scope.as_str(),
            retry_after.as_millis(),
            started.elapsed().as_millis()
        );
        return;
    }

    match classify_session_route(&parsed.path) {
        SessionRoute::Logout => {
            let (verified, _, _) = transport.app().tools().security().auth_with_signature(
                &parsed.user_id,
                &parsed.payload,
                &parsed.signature,
            );
            let message = if verified {
                transport
                    .app()
                    .tools()
                    .signaler()
                    .listeners()
                    .remove(&parsed.user_id);
                "loggedout"
            } else {
                "logout_failed"
            };
            socket.write_response(
                &parsed.packet_id,
                0,
                &serde_json::to_vec(&build_error_json(message)).unwrap_or_default(),
            );
            eprintln!(
                "[{label}] << path=logout pkt={} elapsed_ms={}",
                parsed.packet_id,
                started.elapsed().as_millis()
            );
            return;
        }
        SessionRoute::Authenticate => {
            let (verified, _, _) = transport.app().tools().security().auth_with_signature(
                &parsed.user_id,
                &parsed.payload,
                &parsed.signature,
            );
            if verified {
                transport.attach_user_listener(socket, &parsed.user_id);
                socket.write_response(
                    &parsed.packet_id,
                    0,
                    &serde_json::to_vec(&build_error_json("authenticated")).unwrap_or_default(),
                );
                let update = serde_json::to_vec(&ResponseSimpleMessage {
                    message: "old_queue_end".to_owned(),
                })
                .unwrap_or_default();
                socket.write_update("old_queue_end", &update);
            } else {
                socket.write_response(
                    &parsed.packet_id,
                    4,
                    &serde_json::to_vec(&build_error_json("authentication failed"))
                        .unwrap_or_default(),
                );
            }
            eprintln!(
                "[{label}] << path=authenticate pkt={} elapsed_ms={}",
                parsed.packet_id,
                started.elapsed().as_millis()
            );
            return;
        }
        SessionRoute::Action => {}
    }

    let secure = match transport.app().actor().fetch_secure_action(&parsed.path) {
        Some(action) => action,
        None => {
            socket.write_response(
                &parsed.packet_id,
                1,
                &serde_json::to_vec(&build_error_json("action not found")).unwrap_or_default(),
            );
            eprintln!(
                "[{label}] << path={} pkt={} code=1 action_not_found elapsed_ms={}",
                parsed.path,
                parsed.packet_id,
                started.elapsed().as_millis()
            );
            return;
        }
    };
    let raw_payload = serde_json::from_slice::<Value>(&parsed.payload).unwrap_or(Value::Null);
    let input = match secure.parse_input(label, raw_payload) {
        Ok(input) => input,
        Err(error) => {
            socket.write_response(
                &parsed.packet_id,
                2,
                &serde_json::to_vec(&build_error_json(&error.to_string())).unwrap_or_default(),
            );
            eprintln!(
                "[{label}] << path={} pkt={} code=2 parse_input_err={} elapsed_ms={}",
                parsed.path,
                parsed.packet_id,
                error,
                started.elapsed().as_millis()
            );
            return;
        }
    };

    match secure.securely_act(
        &parsed.user_id,
        &parsed.packet_id,
        &parsed.payload,
        &parsed.signature,
        input,
        &peer_ip,
        &[],
    ) {
        Ok((code, value)) => {
            let response = serde_json::to_vec(&value).unwrap_or_default();
            socket.write_response(&parsed.packet_id, code, &response);
            eprintln!(
                "[{label}] << path={} pkt={} code={} resp_len={} elapsed_ms={}",
                parsed.path,
                parsed.packet_id,
                code,
                response.len(),
                started.elapsed().as_millis()
            );
            if !parsed.user_id.is_empty() && !socket.listener_registered() {
                socket.mark_listener_registered();
                transport.attach_user_listener(socket, &parsed.user_id);
            }
            transport.apply_gateway_subscription(socket, &parsed.path, &value);
        }
        Err(error) => {
            socket.write_response(
                &parsed.packet_id,
                3,
                &serde_json::to_vec(&build_error_json(&error.to_string())).unwrap_or_default(),
            );
            eprintln!(
                "[{label}] << path={} pkt={} code=3 act_err={} elapsed_ms={}",
                parsed.path,
                parsed.packet_id,
                error,
                started.elapsed().as_millis()
            );
        }
    }
}
