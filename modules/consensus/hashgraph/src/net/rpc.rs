//! Translation of `chain/net/rpc.go`.
//!
//! Go modelled `RPC.Command` / `RPCResponse.Response` as `interface{}` and
//! type-asserted them in the consumer. The translation makes them enums.

use crossbeam_channel::Sender;

use super::commands::{
    EagerSyncRequest, EagerSyncResponse, FastForwardRequest, FastForwardResponse, JoinRequest,
    JoinResponse, SyncRequest, SyncResponse,
};

/// An RPC request command.
pub enum RpcCommand {
    Sync(SyncRequest),
    EagerSync(EagerSyncRequest),
    FastForward(FastForwardRequest),
    Join(JoinRequest),
}

/// An RPC response payload.
pub enum RpcResponseKind {
    Sync(SyncResponse),
    EagerSync(EagerSyncResponse),
    FastForward(FastForwardResponse),
    Join(JoinResponse),
}

/// Captures both a response and a potential error.
pub struct RpcResponse {
    pub response: Option<RpcResponseKind>,
    pub error: Option<String>,
}

/// Encapsulates an RPC request and provides a response mechanism.
pub struct Rpc {
    pub command: RpcCommand,
    pub resp_chan: Sender<RpcResponse>,
}

impl Rpc {
    /// Responds with a response, error, or both.
    pub fn respond(&self, response: Option<RpcResponseKind>, error: Option<String>) {
        let _ = self.resp_chan.send(RpcResponse { response, error });
    }
}
