//! Translation of `chain/net/inmem_transport.go`.
//!
//! The `InmemTransport` implements [`Transport`] for in-process testing. Go
//! used a `chan RPC`; the translation uses a crossbeam channel. Peers store
//! each other's consumer-channel sender for local routing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use uuid::Uuid;

use super::commands::{
    EagerSyncRequest, EagerSyncResponse, FastForwardRequest, FastForwardResponse, JoinRequest,
    JoinResponse, SyncRequest, SyncResponse,
};
use super::rpc::{Rpc, RpcCommand, RpcResponse, RpcResponseKind};
use super::transport::Transport;

/// An in-memory [`Transport`] used for testing Babble internally.
pub struct InmemTransport {
    consumer_tx: Sender<Rpc>,
    consumer_rx: Receiver<Rpc>,
    local_addr: String,
    peers: Arc<Mutex<HashMap<String, Sender<Rpc>>>>,
    timeout: Duration,
}

impl InmemTransport {
    /// Initialises a new `InmemTransport`, generating a random local address if
    /// none is given. Returns `(address, transport)`.
    pub fn new(addr: &str) -> (String, InmemTransport) {
        let addr = if addr.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            addr.to_string()
        };
        let (consumer_tx, consumer_rx) = bounded(16);
        let trans = InmemTransport {
            consumer_tx,
            consumer_rx,
            local_addr: addr.clone(),
            peers: Arc::new(Mutex::new(HashMap::new())),
            timeout: Duration::from_millis(50),
        };
        (addr, trans)
    }

    fn make_rpc(&self, target: &str, command: RpcCommand) -> Result<RpcResponse> {
        let peer_tx = self.peers.lock().unwrap().get(target).cloned();
        let peer_tx = peer_tx.ok_or_else(|| anyhow!("failed to connect to peer: {}", target))?;

        let (resp_tx, resp_rx) = bounded(1);
        peer_tx
            .send(Rpc {
                command,
                resp_chan: resp_tx,
            })
            .map_err(|_| anyhow!("failed to send RPC to peer"))?;

        match resp_rx.recv_timeout(self.timeout) {
            Ok(resp) => {
                if let Some(e) = &resp.error {
                    return Err(anyhow!("{}", e));
                }
                Ok(resp)
            }
            Err(_) => Err(anyhow!("command timed out")),
        }
    }

    /// Connects this transport to another for a given peer name (local
    /// routing).
    pub fn connect(&self, peer: &str, t: &InmemTransport) {
        self.peers
            .lock()
            .unwrap()
            .insert(peer.to_string(), t.consumer_tx.clone());
    }

    /// Removes the ability to route to a given peer.
    pub fn disconnect(&self, peer: &str) {
        self.peers.lock().unwrap().remove(peer);
    }

    /// Removes all routes to peers.
    pub fn disconnect_all(&self) {
        self.peers.lock().unwrap().clear();
    }
}

impl Transport for InmemTransport {
    fn listen(&self) {}

    fn consumer(&self, _work_chain_id: &str, _shard_chain_id: &str) -> Receiver<Rpc> {
        self.consumer_rx.clone()
    }

    fn local_addr(&self) -> String {
        self.local_addr.clone()
    }

    fn advertise_addr(&self) -> String {
        self.local_addr.clone()
    }

    fn sync(&self, target: &str, args: &SyncRequest) -> Result<SyncResponse> {
        let resp = self.make_rpc(target, RpcCommand::Sync(args.clone()))?;
        match resp.response {
            Some(RpcResponseKind::Sync(r)) => Ok(r),
            _ => Err(anyhow!("unexpected response type for Sync")),
        }
    }

    fn eager_sync(&self, target: &str, args: &EagerSyncRequest) -> Result<EagerSyncResponse> {
        let resp = self.make_rpc(target, RpcCommand::EagerSync(args.clone()))?;
        match resp.response {
            Some(RpcResponseKind::EagerSync(r)) => Ok(r),
            _ => Err(anyhow!("unexpected response type for EagerSync")),
        }
    }

    fn fast_forward(&self, target: &str, args: &FastForwardRequest) -> Result<FastForwardResponse> {
        let resp = self.make_rpc(target, RpcCommand::FastForward(args.clone()))?;
        match resp.response {
            Some(RpcResponseKind::FastForward(r)) => Ok(r),
            _ => Err(anyhow!("unexpected response type for FastForward")),
        }
    }

    fn join(&self, target: &str, args: &JoinRequest) -> Result<JoinResponse> {
        let resp = self.make_rpc(target, RpcCommand::Join(args.clone()))?;
        match resp.response {
            Some(RpcResponseKind::Join(r)) => Ok(r),
            _ => Err(anyhow!("unexpected response type for Join")),
        }
    }

    fn close(&self) -> Result<()> {
        self.disconnect_all();
        Ok(())
    }
}
