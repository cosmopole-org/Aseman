//! Sending base requests onto the main chain with a response callback.

use std::sync::Arc;

use crate::transports::chain::callbacks::BaseResponseCallback;
use crate::transports::chain::callbacks::ChainCallback;
use crate::transports::chain::globe::Globe;
use crate::transports::chain::globe::types::ChainPacketOp;
use crate::util::crypto::secure_unique_string;
use aseman_contracts::wire::chain::ChainBaseRequest;

impl Globe {
    pub(crate) fn send_base_request_on_chain(
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
        let cb_arc: crate::transports::chain::callbacks::ChainCallbackFn = Arc::from(callback);
        (self.set_chain_callback_fn)(&callback_id, ChainCallback { fn_: cb_arc });
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
}
