//! Translation of `chain/net/transport.go`.

use anyhow::Result;
use crossbeam_channel::Receiver;

use super::commands::{
    EagerSyncRequest, EagerSyncResponse, FastForwardRequest, FastForwardResponse, JoinRequest,
    JoinResponse, SyncRequest, SyncResponse,
};
use super::rpc::Rpc;

/// An interface for network transports allowing a node to communicate with
/// other nodes.
///
/// Go filled the response through an out-pointer argument; the translation
/// returns the response value instead.
pub trait Transport: Send + Sync {
    /// Starts the transport listening.
    fn listen(&self);

    /// Returns a channel used to consume and respond to RPC requests.
    fn consumer(&self, work_chain_id: &str, shard_chain_id: &str) -> Receiver<Rpc>;

    /// Returns our local address.
    fn local_addr(&self) -> String;

    /// Returns our advertise address, where other peers can reach us.
    fn advertise_addr(&self) -> String;

    /// Sends a `SyncRequest` to the target node.
    fn sync(&self, target: &str, args: &SyncRequest) -> Result<SyncResponse>;

    /// Sends an `EagerSyncRequest` to the target node.
    fn eager_sync(&self, target: &str, args: &EagerSyncRequest) -> Result<EagerSyncResponse>;

    /// Sends a `FastForwardRequest` to the target node.
    fn fast_forward(&self, target: &str, args: &FastForwardRequest) -> Result<FastForwardResponse>;

    /// Sends a `JoinRequest` to the target node.
    fn join(&self, target: &str, args: &JoinRequest) -> Result<JoinResponse>;

    /// Permanently closes the transport, freeing its resources.
    fn close(&self) -> Result<()>;
}
