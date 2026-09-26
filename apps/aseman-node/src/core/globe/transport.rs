//! The `IGlobe` transport surface: chain base-request / typed-message /
//! base-response plumbing.
//!
//! Translation of `core/module/globe/globe.go`. Staking and the election round
//! live entirely in the consensus engine's `Governance` (RL-011), driven by the
//! provider's autonomous scheduler and the orchestrator's chain-packet routing;
//! the globe carries no staking/election state or controllers.

use std::collections::HashMap;
use std::sync::Arc;

use crate::api::utils::crypto::secure_unique_string;
use crate::core::globe::Globe;
use crate::core::globe::types::{ChainPacketOp, SubmitChainPacketFn};
use crate::models::chain::{
    ChainBaseRequest, ChainCallback, ChainMessage, ChainPayPacket, ChainResponse, Effects,
    MessageCallback,
};
use crate::models::globe::{BaseResponseCallback, IGlobe, TypedMessageCallback};
use crate::models::update::Update;

impl IGlobe for Globe {
    fn send_base_request_on_chain(
        &self,
        key: &str,
        payload: Vec<u8>,
        signature: &str,
        user_id: &str,
        tag: &str,
        callback: BaseResponseCallback,
    ) {
        let callback_id = secure_unique_string();
        // `BaseResponseCallback` is `Box<dyn Fn>`; ChainCallback wants
        // `Arc<dyn Fn>`. Re-wrap on the way through.
        let cb_arc: crate::models::chain::ChainCallbackFn = Arc::from(callback);
        (self.set_chain_callback_fn)(
            &callback_id,
            ChainCallback {
                fn_: cb_arc,
                executors: HashMap::new(),
                responses: HashMap::new(),
                tag: tag.to_string(),
            },
        );
        let req = ChainBaseRequest {
            key: key.to_string(),
            author: format!("user::{}", user_id),
            submitter: self.node_id.clone(),
            payload: payload.clone(),
            signatures: vec![(self.sign_packet_fn)(&payload), signature.to_string()],
            request_id: callback_id,
            tag: tag.to_string(),
        };
        (self.submit_chain_packet_fn)("main", ChainPacketOp::BaseRequest(req));
    }

    #[allow(clippy::too_many_arguments)]
    fn send_typed_message_on_chain(
        &self,
        chain_id: &str,
        key: &str,
        message_type: &str,
        payload: Vec<u8>,
        signature: &str,
        user_id: &str,
        receivers: HashMap<String, HashMap<String, bool>>,
        reply_to: &str,
        store_id: &str,
        pay: Option<ChainPayPacket>,
        callback: Option<TypedMessageCallback>,
    ) {
        let callback_id = secure_unique_string();
        // Register a reply callback only when the caller actually wants one.
        // Fire-and-forget sends pass `None`, so no entry is parked in
        // `message_callbacks` — previously every send left a permanent entry
        // there (it was never removed on delivery either), an unbounded leak on
        // the on-chain messaging path.
        if let Some(callback) = callback {
            let cb_arc: Arc<dyn Fn(String, Vec<u8>) + Send + Sync> = Arc::from(callback);
            (self.set_message_cb_fn)(
                &callback_id,
                MessageCallback {
                    id: callback_id.clone(),
                    fn_: cb_arc,
                },
            );
        }
        let chain_id = if chain_id.is_empty() {
            "main"
        } else {
            chain_id
        };
        let msg = ChainMessage {
            key: key.to_string(),
            message_type: message_type.to_string(),
            author: format!("user::{}", user_id),
            submitter: self.node_id.clone(),
            payload: payload.clone(),
            signatures: vec![(self.sign_packet_fn)(&payload), signature.to_string()],
            request_id: callback_id,
            recievers: receivers,
            reply_to: reply_to.to_string(),
            store_id: store_id.to_string(),
            pay,
        };
        (self.submit_chain_packet_fn)(chain_id, ChainPacketOp::Message(msg));
    }

    #[allow(clippy::too_many_arguments)]
    fn exec_base_response_on_chain(
        &self,
        callback_id: &str,
        packet: Vec<u8>,
        signature: &str,
        res_code: i64,
        e: &str,
        mut updates: Vec<Update>,
        tag: &str,
        to_user_id: &str,
    ) {
        updates.sort_by(|a, b| {
            let ka = format!("{}:{}", a.typ, a.key);
            let kb = format!("{}:{}", b.typ, b.key);
            ka.cmp(&kb)
        });
        let resp = ChainResponse {
            to_user_id: to_user_id.to_string(),
            tag: tag.to_string(),
            signature: signature.to_string(),
            executor: self.node_id.clone(),
            request_id: callback_id.to_string(),
            res_code,
            err: e.to_string(),
            payload: packet,
            effects: Effects {
                db_updates: updates,
            },
        };
        (self.submit_chain_packet_fn)("main", ChainPacketOp::Response(resp));
    }
}

/// Kept for clippy calm on the `SubmitChainPacketFn` import in this module.
pub(crate) fn _anyval_submit_hint(_: SubmitChainPacketFn) {}
const _: fn() -> Option<chrono::DateTime<chrono::Utc>> = || {
    use chrono::TimeZone;
    chrono::Utc.timestamp_opt(0, 0).single()
};
