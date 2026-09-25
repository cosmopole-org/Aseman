use std::collections::HashMap;
use std::time::SystemTime;

use crate::models::chain::ChainPayPacket;
use crate::models::update::Update;
use crate::util::GoError;

/// Callback receiving a chain base-request response.
pub type BaseResponseCallback = Box<dyn Fn(Vec<u8>, i64, Option<GoError>) + Send + Sync>;

/// Callback receiving a typed chain message reply.
pub type TypedMessageCallback = Box<dyn Fn(String, Vec<u8>) + Send + Sync>;

/// The "globe" — the node's window onto the global chain network.
pub trait IGlobe: Send + Sync {
    fn send_base_request_on_chain(
        &self,
        key: &str,
        payload: Vec<u8>,
        signature: &str,
        user_id: &str,
        tag: &str,
        callback: BaseResponseCallback,
    );
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
        // `None` for fire-and-forget sends that never expect a reply. A `Some`
        // callback is registered one-shot and dropped the moment its reply is
        // delivered; passing `None` avoids parking a callback that would never
        // be reaped (the source of an unbounded `message_callbacks` leak).
        callback: Option<TypedMessageCallback>,
    );
    #[allow(clippy::too_many_arguments)]
    fn exec_base_response_on_chain(
        &self,
        callback_id: &str,
        packet: Vec<u8>,
        signature: &str,
        res_code: i64,
        e: &str,
        updates: Vec<Update>,
        tag: &str,
        to_user_id: &str,
    );
    fn handle(&self, typ: &str, trx_payload: Vec<u8>) -> bool;
    fn stake_node_owner(&self, node_id: &str, owner_id: &str, amount: i64);
    fn try_start_scheduled_election(&self, now: SystemTime);
}
