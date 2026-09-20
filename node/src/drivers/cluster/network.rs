//! Raft RPC transport between Caspar instances: plain HTTP + JSON.
//!
//! Each instance runs the cluster HTTP listener (see `server.rs`); this
//! module is the client side used by openraft's replication machinery. The
//! server answers with the serialized `Result<Resp, RaftError>` so remote
//! raft-level errors round-trip losslessly.

use openraft::error::{InstallSnapshotError, RaftError, RemoteError, Unreachable};
use openraft::network::{RPCOption, RaftNetwork, RaftNetworkFactory};
use openraft::raft::{
    AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest, InstallSnapshotResponse,
    VoteRequest, VoteResponse,
};
use openraft::BasicNode;
use serde::de::DeserializeOwned;
use serde::Serialize;

use super::command::TypeConfig;

type NodeId = u64;

/// Builds one HTTP client per replication target.
pub struct HttpNetworkFactory {
    pub auth_token: String,
}

pub struct HttpRaftClient {
    target: NodeId,
    addr: String,
    auth_token: String,
    client: reqwest::Client,
}

impl HttpRaftClient {
    async fn rpc<Req, Resp, Err>(
        &self,
        uri: &str,
        req: &Req,
        option: &RPCOption,
    ) -> Result<Resp, openraft::error::RPCError<NodeId, BasicNode, RaftError<NodeId, Err>>>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
        Err: std::error::Error + DeserializeOwned,
    {
        let url = format!("http://{}/raft/{}", self.addr, uri);
        let mut builder = self.client.post(&url).timeout(option.hard_ttl()).json(req);
        if !self.auth_token.is_empty() {
            builder = builder.header("x-caspar-cluster-token", &self.auth_token);
        }
        let resp = builder.send().await.map_err(|e| Unreachable::new(&e))?;
        let result: Result<Resp, RaftError<NodeId, Err>> =
            resp.json().await.map_err(|e| Unreachable::new(&e))?;
        result.map_err(|e| RemoteError::new(self.target, e).into())
    }
}

impl RaftNetworkFactory<TypeConfig> for HttpNetworkFactory {
    type Network = HttpRaftClient;

    async fn new_client(&mut self, target: NodeId, node: &BasicNode) -> Self::Network {
        HttpRaftClient {
            target,
            addr: node.addr.clone(),
            auth_token: self.auth_token.clone(),
            client: reqwest::Client::new(),
        }
    }
}

impl RaftNetwork<TypeConfig> for HttpRaftClient {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<
        AppendEntriesResponse<NodeId>,
        openraft::error::RPCError<NodeId, BasicNode, RaftError<NodeId>>,
    > {
        self.rpc("append", &rpc, &option).await
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<NodeId>,
        openraft::error::RPCError<NodeId, BasicNode, RaftError<NodeId, InstallSnapshotError>>,
    > {
        self.rpc("snapshot", &rpc, &option).await
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, openraft::error::RPCError<NodeId, BasicNode, RaftError<NodeId>>>
    {
        self.rpc("vote", &rpc, &option).await
    }
}
