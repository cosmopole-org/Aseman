//! Translation of `chain/node/node_rpc.go` — the node's RPC request senders
//! and incoming-request handlers.

use std::collections::HashMap;

use anyhow::Result;

use super::node::Node;
use super::state::State;
use crate::hashgraph::{InternalTransaction, WireEvent};
use crate::net::{
    EagerSyncRequest, EagerSyncResponse, FastForwardRequest, FastForwardResponse, JoinRequest,
    JoinResponse, Rpc, RpcCommand, RpcResponseKind, SyncRequest, SyncResponse,
};
use crate::peers::Peer;

impl Node {
    pub(crate) fn request_sync(
        &self,
        target: &str,
        known: HashMap<u32, i64>,
        sync_limit: i64,
    ) -> Result<SyncResponse> {
        let args = SyncRequest {
            from_id: self.core.lock().unwrap().validator.id(),
            sync_limit,
            known,
            work_chain_id: self.workchain_id.clone(),
            shard_chain_id: self.shardchain_id.clone(),
        };
        self.trans.sync(target, &args)
    }

    pub(crate) fn request_eager_sync(
        &self,
        target: &str,
        events: Vec<WireEvent>,
    ) -> Result<EagerSyncResponse> {
        let args = EagerSyncRequest {
            from_id: self.core.lock().unwrap().validator.id(),
            events,
            work_chain_id: self.workchain_id.clone(),
            shard_chain_id: self.shardchain_id.clone(),
        };
        self.trans.eager_sync(target, &args)
    }

    pub(crate) fn request_fast_forward(&self, target: &str) -> Result<FastForwardResponse> {
        self.logger
            .with_field("target", target)
            .debug("RequestFastForward()");
        let args = FastForwardRequest {
            from_id: self.core.lock().unwrap().validator.id(),
            work_chain_id: self.workchain_id.clone(),
            shard_chain_id: self.shardchain_id.clone(),
        };
        self.trans.fast_forward(target, &args)
    }

    pub(crate) fn request_join(&self, target: &str) -> Result<JoinResponse> {
        let (pub_hex, moniker, key) = {
            let core = self.core.lock().unwrap();
            (
                core.validator.public_key_hex(),
                core.validator.moniker.clone(),
                core.validator.key.clone(),
            )
        };
        let peer = Peer::new(&pub_hex, &self.trans.advertise_addr(), &moniker);
        let mut join_tx = InternalTransaction::new_join(peer);
        let _ = join_tx.sign(&key);

        let args = JoinRequest {
            internal_transaction: join_tx,
            work_chain_id: self.workchain_id.clone(),
            shard_chain_id: self.shardchain_id.clone(),
        };
        self.trans.join(target, &args)
    }

    /// Dispatches an incoming RPC to the appropriate handler.
    pub fn process_rpc(&self, rpc: Rpc) {
        let is_sync_request = matches!(rpc.command, RpcCommand::Sync(_));
        let state = self.manager.get_state();
        if !(state == State::Babbling || (state == State::Suspended && is_sync_request)) {
            self.logger
                .with_field("state", state.to_string())
                .debug("Not in Babbling state");
            rpc.respond(None, Some("Not in Babbling state".to_string()));
            return;
        }

        match &rpc.command {
            RpcCommand::Sync(cmd) => self.process_sync_request(&rpc, cmd),
            RpcCommand::EagerSync(cmd) => self.process_eager_sync_request(&rpc, cmd),
            RpcCommand::FastForward(cmd) => self.process_fast_forward_request(&rpc, cmd),
            RpcCommand::Join(cmd) => self.process_join_request(&rpc, cmd),
        }
    }

    fn process_sync_request(&self, rpc: &Rpc, cmd: &SyncRequest) {
        self.logger
            .with_field("from_id", cmd.from_id)
            .with_field("sync_limit", cmd.sync_limit)
            .debug("process SyncRequest");

        let mut resp = SyncResponse {
            from_id: self.core.lock().unwrap().validator.id(),
            ..SyncResponse::default()
        };
        let mut resp_err: Option<String> = None;

        let event_diff = {
            let mut core = self.core.lock().unwrap();
            core.event_diff(cmd.known.clone())
        };

        match event_diff {
            Err(e) => {
                self.logger.with_error(&e).error("Calculating Diff");
                resp_err = Some(e.to_string());
            }
            Ok(mut event_diff) => {
                if !event_diff.is_empty() {
                    let limit = cmd.sync_limit.min(self.conf.sync_limit);
                    if (limit as usize) < event_diff.len() {
                        event_diff.truncate(limit.max(0) as usize);
                    }
                    let wire_events = self.core.lock().unwrap().to_wire(&event_diff);
                    resp.events = wire_events;
                }
            }
        }

        resp.known = self.core.lock().unwrap().known_events();
        self.logger
            .with_field("events", resp.events.len())
            .debug("Responding to SyncRequest");
        rpc.respond(Some(RpcResponseKind::Sync(resp)), resp_err);
    }

    fn process_eager_sync_request(&self, rpc: &Rpc, cmd: &EagerSyncRequest) {
        self.logger
            .with_field("from_id", cmd.from_id)
            .with_field("events", cmd.events.len())
            .debug("EagerSyncRequest");

        let mut success = true;
        let err = {
            let mut core = self.core.lock().unwrap();
            self.sync_with_core(&mut core, cmd.from_id, cmd.events.clone())
        };
        if let Err(e) = &err {
            self.logger.with_error(e).error("sync()");
            success = false;
        }

        let resp = EagerSyncResponse {
            from_id: self.core.lock().unwrap().validator.id(),
            success,
            ..EagerSyncResponse::default()
        };
        rpc.respond(
            Some(RpcResponseKind::EagerSync(resp)),
            err.err().map(|e| e.to_string()),
        );
    }

    fn process_fast_forward_request(&self, rpc: &Rpc, cmd: &FastForwardRequest) {
        self.logger
            .with_field("from", cmd.from_id)
            .debug("process FastForwardRequest");

        let mut resp = FastForwardResponse {
            from_id: self.core.lock().unwrap().validator.id(),
            ..FastForwardResponse::default()
        };
        let mut resp_err: Option<String> = None;

        let anchor = {
            let mut core = self.core.lock().unwrap();
            core.get_anchor_block_with_frame()
        };

        match anchor {
            Err(e) => {
                self.logger.with_error(&e).error("Getting Frame");
                resp_err = Some(e.to_string());
            }
            Ok((block, frame)) => {
                let block_index = block.index();
                resp.block = block;
                resp.frame = frame;
                match self.proxy.get_snapshot(block_index) {
                    Ok(snapshot) => resp.snapshot = snapshot,
                    Err(e) => {
                        self.logger.with_error(&e).error("Getting Snapshot");
                        resp_err = Some(e.to_string());
                    }
                }
            }
        }

        rpc.respond(Some(RpcResponseKind::FastForward(resp)), resp_err);
    }

    fn process_join_request(&self, rpc: &Rpc, cmd: &JoinRequest) {
        self.logger.debug("process JoinRequest");

        let mut resp_err: Option<String> = None;
        let mut accepted = false;
        let mut accepted_round: i64 = 0;
        let mut peers: Vec<Peer> = Vec::new();

        let verified = cmd.internal_transaction.verify().unwrap_or(false);
        if !verified {
            let msg = "Unable to verify signature on join request";
            self.logger.debug(msg);
            resp_err = Some(msg.to_string());
        } else {
            let already_present = {
                let core = self.core.lock().unwrap();
                core.peers
                    .by_pub_key
                    .contains_key(&cmd.internal_transaction.body.peer.pub_key_string())
            };
            if already_present {
                self.logger.debug("JoinRequest peer is already present");
                accepted = true;
                let core = self.core.lock().unwrap();
                if let Some(lcr) = core.get_last_consensus_round_index() {
                    accepted_round = lcr;
                }
                peers = core.peers.peers.clone();
            } else {
                let resp_rx = {
                    let mut core = self.core.lock().unwrap();
                    core.add_internal_transaction(cmd.internal_transaction.clone())
                };
                match resp_rx.recv_timeout(self.conf.join_timeout) {
                    Ok(resp) => {
                        accepted = resp.accepted;
                        accepted_round = resp.accepted_round;
                        peers = resp.peers;
                    }
                    Err(_) => {
                        let msg = "Timeout waiting for JoinRequest to go through consensus";
                        self.logger.error(msg);
                        resp_err = Some(msg.to_string());
                    }
                }
            }
        }

        let resp = JoinResponse {
            from_id: self.core.lock().unwrap().validator.id(),
            accepted,
            accepted_round,
            peers,
            ..JoinResponse::default()
        };
        self.logger
            .with_field("accepted", resp.accepted)
            .with_field("accepted_round", resp.accepted_round)
            .with_field("peers", resp.peers.len())
            .debug("Responding to JoinRequest");
        rpc.respond(Some(RpcResponseKind::Join(resp)), resp_err);

        let host = cmd
            .internal_transaction
            .body
            .peer
            .net_addr
            .split(':')
            .next()
            .unwrap_or("")
            .to_string();
        (self.on_new_node)(host);
    }
}
