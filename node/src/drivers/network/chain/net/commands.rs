//! Translation of `chain/net/commands.go` — the RPC request/response types.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::drivers::network::chain::hashgraph::{Block, Frame, InternalTransaction, WireEvent};
use crate::drivers::network::chain::peers::Peer;

/// The pull part of the pull-push gossip protocol: retrieves unknown Events
/// from another node.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct SyncRequest {
    #[serde(rename = "FromID")]
    pub from_id: u32,
    pub known: HashMap<u32, i64>,
    pub sync_limit: i64,
    pub work_chain_id: String,
    pub shard_chain_id: String,
}

/// Returns a list of Events as requested by a [`SyncRequest`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct SyncResponse {
    #[serde(rename = "FromID")]
    pub from_id: u32,
    pub events: Vec<WireEvent>,
    pub known: HashMap<u32, i64>,
    pub work_chain_id: String,
    pub shard_chain_id: String,
}

/// The push part of the pull-push gossip protocol: actively pushes Events to a
/// node.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct EagerSyncRequest {
    #[serde(rename = "FromID")]
    pub from_id: u32,
    pub events: Vec<WireEvent>,
    pub work_chain_id: String,
    pub shard_chain_id: String,
}

/// Indicates the success or failure of an [`EagerSyncRequest`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct EagerSyncResponse {
    #[serde(rename = "FromID")]
    pub from_id: u32,
    pub success: bool,
    pub work_chain_id: String,
    pub shard_chain_id: String,
}

/// Requests a Block, Frame and Snapshot to fast-forward from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct FastForwardRequest {
    #[serde(rename = "FromID")]
    pub from_id: u32,
    pub work_chain_id: String,
    pub shard_chain_id: String,
}

/// Encapsulates the response to a [`FastForwardRequest`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct FastForwardResponse {
    #[serde(rename = "FromID")]
    pub from_id: u32,
    pub block: Block,
    pub frame: Frame,
    #[serde(with = "crate::util::bytes_base64")]
    pub snapshot: Vec<u8>,
    pub work_chain_id: String,
    pub shard_chain_id: String,
}

/// Submits an InternalTransaction to join a Babble group.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct JoinRequest {
    pub internal_transaction: InternalTransaction,
    pub work_chain_id: String,
    pub shard_chain_id: String,
}

/// The response to a [`JoinRequest`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct JoinResponse {
    #[serde(rename = "FromID")]
    pub from_id: u32,
    pub accepted: bool,
    pub accepted_round: i64,
    pub peers: Vec<Peer>,
    pub work_chain_id: String,
    pub shard_chain_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drivers::network::chain::hashgraph::{InternalTransaction, TransactionType};

    fn fresh_peer() -> Peer {
        use crate::drivers::network::chain::crypto::keys::{generate_ecdsa_key, public_key_hex};
        let key = generate_ecdsa_key().unwrap();
        Peer::new(&public_key_hex(key.verifying_key()), "addr", "m")
    }

    #[test]
    fn sync_request_uses_uppercase_from_id_and_round_trips() {
        let mut req = SyncRequest {
            from_id: 17,
            sync_limit: 100,
            work_chain_id: "w".to_string(),
            shard_chain_id: "s".to_string(),
            ..SyncRequest::default()
        };
        req.known.insert(1, 5);
        req.known.insert(2, 6);

        let bytes = serde_json::to_vec(&req).unwrap();
        let raw = std::str::from_utf8(&bytes).unwrap();
        assert!(
            raw.contains(r#""FromID":17"#),
            "expected FromID, got {}",
            raw
        );
        assert!(raw.contains(r#""WorkChainId""#));

        let parsed: SyncRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.from_id, req.from_id);
        assert_eq!(parsed.sync_limit, req.sync_limit);
        assert_eq!(parsed.known, req.known);
    }

    #[test]
    fn sync_response_round_trip_preserves_events_field() {
        let resp = SyncResponse {
            from_id: 3,
            events: Vec::new(),
            work_chain_id: "w".to_string(),
            shard_chain_id: "s".to_string(),
            ..SyncResponse::default()
        };
        let bytes = serde_json::to_vec(&resp).unwrap();
        let parsed: SyncResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.from_id, 3);
        assert!(parsed.events.is_empty());
    }

    #[test]
    fn eager_sync_request_response_round_trip() {
        let req = EagerSyncRequest {
            from_id: 1,
            events: Vec::new(),
            work_chain_id: "w".to_string(),
            shard_chain_id: "s".to_string(),
        };
        let bytes = serde_json::to_vec(&req).unwrap();
        let parsed: EagerSyncRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.from_id, req.from_id);
        assert_eq!(parsed.work_chain_id, req.work_chain_id);

        let resp = EagerSyncResponse {
            from_id: 2,
            success: true,
            work_chain_id: "w".to_string(),
            shard_chain_id: "s".to_string(),
        };
        let bytes = serde_json::to_vec(&resp).unwrap();
        let parsed: EagerSyncResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(parsed.success);
        assert_eq!(parsed.from_id, 2);
    }

    #[test]
    fn fast_forward_request_and_response_round_trip_with_snapshot_base64() {
        let req = FastForwardRequest {
            from_id: 9,
            work_chain_id: "w".to_string(),
            shard_chain_id: "s".to_string(),
        };
        let bytes = serde_json::to_vec(&req).unwrap();
        let parsed: FastForwardRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.from_id, 9);

        let snapshot = vec![0u8, 1, 2, 3, 255];
        let resp = FastForwardResponse {
            from_id: 9,
            block: Default::default(),
            frame: Frame::default(),
            snapshot: snapshot.clone(),
            work_chain_id: "w".to_string(),
            shard_chain_id: "s".to_string(),
        };
        let bytes = serde_json::to_vec(&resp).unwrap();
        let raw = std::str::from_utf8(&bytes).unwrap();
        // The snapshot field is base64-encoded (`AAECA/8=`).
        assert!(
            raw.contains(r#""Snapshot":"AAECA/8=""#),
            "snapshot encoding: {}",
            raw
        );

        let parsed: FastForwardResponse = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.snapshot, snapshot);
    }

    #[test]
    fn join_request_response_round_trip_with_internal_transaction() {
        let itx = InternalTransaction::new(TransactionType::PeerAdd, fresh_peer());
        let req = JoinRequest {
            internal_transaction: itx.clone(),
            work_chain_id: "w".to_string(),
            shard_chain_id: "s".to_string(),
        };
        let bytes = serde_json::to_vec(&req).unwrap();
        let parsed: JoinRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(parsed.internal_transaction, itx);

        let resp = JoinResponse {
            from_id: 5,
            accepted: true,
            accepted_round: 12,
            peers: vec![fresh_peer(), fresh_peer()],
            work_chain_id: "w".to_string(),
            shard_chain_id: "s".to_string(),
        };
        let bytes = serde_json::to_vec(&resp).unwrap();
        let parsed: JoinResponse = serde_json::from_slice(&bytes).unwrap();
        assert!(parsed.accepted);
        assert_eq!(parsed.accepted_round, 12);
        assert_eq!(parsed.peers.len(), 2);
    }

    #[test]
    fn missing_optional_fields_default_to_zero_via_serde_default() {
        // An empty object should deserialize fine because every field uses
        // serde(default).
        let req: SyncRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(req.from_id, 0);
        assert!(req.known.is_empty());
    }
}
