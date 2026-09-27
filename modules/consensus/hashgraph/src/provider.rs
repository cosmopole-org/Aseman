//! A804 finance-consensus port over the real Babble application proxy.
//!
//! The provider is the consensus engine's **application handler** (RL-011): a
//! single [`ProxyHandler`] owns the finance ledger and the validator governance
//! (staking + election) from committed blocks, and only request/response/message
//! transactions are forwarded to the chain module's registered pipeline.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::{Result as AnyResult, anyhow};
use aseman_domain::consensus::{Checkpoint, Epoch, Finalized};
use aseman_ports::consensus::ConsensusProvider;
use aseman_ports::{PortError, PortResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::governance::Governance;
use crate::hashgraph::Block;
use crate::node::state::State as NodeState;
use crate::proxy::{CommitResponse, InmemProxy, ProxyHandler};

const PROVIDER_NAME: &str = "hashgraph";

/// A committed app transaction forwarded to the chain module (request/response/
/// message only). The transaction is the raw `typ::payload` frame the chain
/// pipeline understands.
pub type ChainForwarder = Arc<dyn Fn(Vec<Vec<u8>>) -> Vec<String> + Send + Sync>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct OrderedRecord {
    idempotency_key: String,
    digest: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ProviderState {
    pending: BTreeMap<String, String>,
    finalized: Vec<Finalized>,
    adopted: Option<Checkpoint>,
}

impl ProviderState {
    fn epoch(&self) -> Epoch {
        self.finalized
            .iter()
            .map(|item| item.epoch)
            .max()
            .unwrap_or(Epoch::GENESIS)
    }

    fn checkpoint(&self, taken_at_millis: i64) -> Checkpoint {
        let mut hash = Sha256::new();
        let adopted_count = self.adopted.as_ref().map_or(0, |item| {
            hash.update(item.digest.as_bytes());
            item.record_count
        });
        for record in &self.finalized {
            hash.update(record.idempotency_key.as_bytes());
            hash.update([0]);
            hash.update(record.epoch.get().to_be_bytes());
            hash.update(record.position.to_be_bytes());
            hash.update(record.digest.as_bytes());
            hash.update(*b"\n");
        }
        Checkpoint {
            epoch: self.epoch(),
            record_count: adopted_count.saturating_add(self.finalized.len() as u64),
            digest: format!("sha256:{}", hex::encode(hash.finalize())),
            taken_at_millis,
        }
    }
}

/// The provider's application handler: finance records, stake packets, and
/// election packets are owned here; everything else (request/response/message)
/// is forwarded to the chain module's registered pipeline.
struct ConsensusHandler {
    state: Arc<Mutex<ProviderState>>,
    governance: Arc<Governance>,
    forwarder: Mutex<Option<ChainForwarder>>,
}

impl ConsensusHandler {
    /// Install the chain-module forwarder for request/response/message txs.
    fn set_forwarder(&self, forwarder: ChainForwarder) {
        *self.forwarder.lock().unwrap() = Some(forwarder);
    }

    /// Classify a committed transaction by its `typ::payload` frame.
    fn classify<'a>(&self, bytes: &'a [u8]) -> (&'a [u8], &'a [u8]) {
        for (i, &b) in bytes.iter().enumerate() {
            if b == b':' && i + 1 < bytes.len() && bytes[i + 1] == b':' {
                return (&bytes[..i], &bytes[i + 2..]);
            }
        }
        (&[], bytes)
    }
}

impl ProxyHandler for ConsensusHandler {
    fn commit_handler(&self, block: Block) -> AnyResult<CommitResponse> {
        let epoch_number = u64::try_from(block.index())
            .map_err(|_| anyhow!("Hashgraph committed a negative block index"))?
            .checked_add(1)
            .ok_or_else(|| anyhow!("Hashgraph block index exhausted the epoch range"))?;
        let epoch = Epoch::from_stored(epoch_number);
        let transactions = block.transactions().to_vec();
        let receipts = block
            .internal_transactions()
            .iter()
            .map(|transaction| transaction.as_accepted())
            .collect::<Vec<_>>();

        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("consensus state poisoned"))?;

        let mut forwarded: Vec<Vec<u8>> = Vec::new();
        for (position, bytes) in transactions.iter().enumerate() {
            let (typ, payload) = self.classify(bytes);
            match typ {
                b"stake" => {
                    // Governance-owned: feed the staking ledger.
                    let pkt: aseman_contracts::legacy_wire::chain::ChainStakePacket =
                        match serde_json::from_slice(payload) {
                            Ok(p) => p,
                            Err(error) => {
                                return Err(anyhow!("invalid stake packet: {error}"));
                            }
                        };
                    self.governance.handle_stake(&pkt);
                }
                b"election" => {
                    // Governance-owned: advance the election round.
                    let pkt: aseman_contracts::legacy_wire::chain::ChainElectionPacket =
                        match serde_json::from_slice(payload) {
                            Ok(p) => p,
                            Err(error) => {
                                return Err(anyhow!("invalid election packet: {error}"));
                            }
                        };
                    self.governance.handle_election(&pkt);
                }
                b"base" | b"message" | b"response" => {
                    // Chain-module-owned: forward to the registered pipeline.
                    forwarded.push(bytes.clone());
                }
                _ => {
                    // Unframed finance record.
                    let record: OrderedRecord = serde_json::from_slice(bytes)
                        .map_err(|error| anyhow!("invalid finance consensus record: {error}"))?;
                    if let Some(existing) = state
                        .finalized
                        .iter()
                        .find(|item| item.idempotency_key == record.idempotency_key)
                    {
                        if existing.digest != record.digest {
                            return Err(anyhow!("idempotency key finalized with another digest"));
                        }
                        state.pending.remove(&record.idempotency_key);
                        continue;
                    }
                    state.pending.remove(&record.idempotency_key);
                    state.finalized.push(Finalized {
                        idempotency_key: record.idempotency_key,
                        epoch,
                        position: u64::try_from(position)
                            .map_err(|_| anyhow!("position overflow"))?,
                        digest: record.digest,
                    });
                }
            }
        }

        if !forwarded.is_empty()
            && let Some(forwarder) = self.forwarder.lock().unwrap().as_ref()
        {
            let _ = forwarder(forwarded);
        }

        let state_hash = state.checkpoint(block.timestamp()).digest.into_bytes();
        Ok(CommitResponse {
            state_hash,
            internal_transaction_receipts: receipts,
        })
    }

    fn snapshot_handler(&self, _block_index: i64) -> AnyResult<Vec<u8>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("consensus state poisoned"))?;
        serde_json::to_vec(&*state).map_err(Into::into)
    }

    fn restore_handler(&self, snapshot: &[u8]) -> AnyResult<Vec<u8>> {
        let restored: ProviderState = serde_json::from_slice(snapshot)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("consensus state poisoned"))?;
        *state = restored;
        Ok(state.checkpoint(0).digest.into_bytes())
    }

    fn state_change_handler(&self, _state: NodeState) -> AnyResult<()> {
        Ok(())
    }
}

/// Finance ordering + validator governance backed by the proxy consumed by a
/// [`crate::babble::Babble`] engine.
pub struct HashgraphConsensusProvider {
    state: Arc<Mutex<ProviderState>>,
    proxy: Arc<InmemProxy>,
    governance: Arc<Governance>,
    handler: Arc<ConsensusHandler>,
}

impl HashgraphConsensusProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::for_node("node", "node")
    }

    /// Create a provider whose governance casts the given node's identity
    /// (`node_id` is used for the node's own commits; `voter_id` is the
    /// identity it presents in election packets).
    #[must_use]
    pub fn for_node(node_id: &str, voter_id: &str) -> Self {
        let state = Arc::new(Mutex::new(ProviderState::default()));
        let governance = Arc::new(Governance::new(node_id, voter_id));
        let handler = Arc::new(ConsensusHandler {
            state: Arc::clone(&state),
            governance: Arc::clone(&governance),
            forwarder: Mutex::new(None),
        });
        Self {
            state,
            proxy: Arc::new(InmemProxy::new(handler.clone(), None)),
            governance,
            handler,
        }
    }

    /// The application proxy that must be installed in the Babble configuration.
    #[must_use]
    pub fn proxy(&self) -> Arc<InmemProxy> {
        Arc::clone(&self.proxy)
    }

    /// The validator-staking / election governance state owned by this provider.
    #[must_use]
    pub fn governance(&self) -> Arc<Governance> {
        Arc::clone(&self.governance)
    }

    /// Register the chain module's request/response/message forwarder.
    ///
    /// Only `base`/`message`/`response` transactions reach it; governance and
    /// finance are owned entirely by this provider.
    pub fn set_chain_forwarder(&self, forwarder: ChainForwarder) {
        self.handler.set_forwarder(forwarder);
    }

    /// Wire the governance's outbound election packets into this provider's own
    /// chain edge (framed `election::`), so the provider fully owns submission.
    pub fn wire_governance_submit(&self) {
        let proxy = Arc::clone(&self.proxy);
        let hook: crate::governance::SubmitElectionFn = Arc::new(
            move |pkt: aseman_contracts::legacy_wire::chain::ChainElectionPacket| {
                let bytes = match serde_json::to_vec(&pkt) {
                    Ok(bytes) => bytes,
                    Err(_) => return,
                };
                let mut framed = Vec::with_capacity(bytes.len() + 10);
                framed.extend_from_slice(b"election::");
                framed.extend_from_slice(&bytes);
                let _ = proxy.submit_tx(&framed);
            },
        );
        self.governance.set_submit(hook);
    }

    /// Start the autonomous hourly-election scheduler.
    ///
    /// Governance is fully self-contained: the provider checks the clock every
    /// second and starts a scheduled election on the hour. The node never drives
    /// elections — it only configures the provider via `ConsensusProvider::set`.
    pub fn spawn_election_scheduler(&self) {
        let governance = Arc::clone(&self.governance);
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(1));
                let _ = governance.start_scheduled_election(SystemTime::now());
            }
        });
    }

    fn lock(&self) -> PortResult<std::sync::MutexGuard<'_, ProviderState>> {
        self.state
            .lock()
            .map_err(|_| PortError::Unavailable("hashgraph consensus state"))
    }
}

impl Default for HashgraphConsensusProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsensusProvider for HashgraphConsensusProvider {
    fn name(&self) -> &str {
        PROVIDER_NAME
    }

    fn submit(&self, idempotency_key: &str, digest: &str) -> PortResult<()> {
        if idempotency_key.is_empty()
            || !digest.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(PortError::Denied("invalid consensus record"));
        }
        {
            let mut state = self.lock()?;
            if let Some(existing) = state
                .finalized
                .iter()
                .find(|item| item.idempotency_key == idempotency_key)
            {
                return if existing.digest == digest {
                    Ok(())
                } else {
                    Err(PortError::Conflict)
                };
            }
            if let Some(existing) = state.pending.get(idempotency_key) {
                return if existing == digest {
                    Ok(())
                } else {
                    Err(PortError::Conflict)
                };
            }
            state
                .pending
                .insert(idempotency_key.to_owned(), digest.to_owned());
        }
        let bytes = serde_json::to_vec(&OrderedRecord {
            idempotency_key: idempotency_key.to_owned(),
            digest: digest.to_owned(),
        })
        .map_err(|error| PortError::Failed(error.to_string()))?;
        if let Err(error) = self.proxy.submit_tx(&bytes) {
            self.lock()?.pending.remove(idempotency_key);
            return Err(PortError::Failed(error.to_string()));
        }
        Ok(())
    }

    fn finalized_epoch(&self) -> PortResult<Epoch> {
        Ok(self.lock()?.epoch())
    }

    fn finalized_after(&self, epoch: Epoch, limit: usize) -> PortResult<Vec<Finalized>> {
        Ok(self
            .lock()?
            .finalized
            .iter()
            .filter(|item| item.epoch > epoch)
            .take(limit.max(1))
            .cloned()
            .collect())
    }

    fn pending(&self) -> PortResult<u64> {
        u64::try_from(self.lock()?.pending.len())
            .map_err(|error| PortError::Failed(error.to_string()))
    }

    fn checkpoint(&self, at_millis: i64) -> PortResult<Checkpoint> {
        Ok(self.lock()?.checkpoint(at_millis))
    }

    fn adopt(&self, checkpoint: &Checkpoint) -> PortResult<()> {
        let mut state = self.lock()?;
        if state.adopted.is_some() || !state.finalized.is_empty() || !state.pending.is_empty() {
            return Err(PortError::Conflict);
        }
        if !checkpoint
            .digest
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(PortError::Denied("invalid consensus checkpoint"));
        }
        state.adopted = Some(checkpoint.clone());
        Ok(())
    }

    fn set(&self, key: &str, value: &str) -> PortResult<()> {
        self.governance.set(key, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::AppProxy;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn finance_records_finalize_and_forward_chain_packets() {
        let provider = HashgraphConsensusProvider::new();
        let forwarded = Arc::new(Mutex::new(Vec::new()));
        let sink = forwarded.clone();
        provider.set_chain_forwarder(Arc::new(move |txs: Vec<Vec<u8>>| {
            sink.lock().unwrap().extend(txs);
            Vec::new()
        }));
        provider.submit("settlement-1", DIGEST).unwrap();
        provider.submit("settlement-1", DIGEST).unwrap();
        assert_eq!(provider.pending().unwrap(), 1);
        let proxy = provider.proxy();
        let transaction = proxy.submit_ch().recv().unwrap();
        // A chain-module message rides the same proxy.
        proxy
            .submit_tx(b"message::{\"key\":\"vm.execute\"}".as_slice())
            .unwrap();
        proxy
            .commit_block(Block::new(
                0,
                0,
                vec![],
                vec![],
                vec![transaction],
                vec![],
                1,
            ))
            .unwrap();
        proxy
            .commit_block(Block::new(
                1,
                0,
                vec![],
                vec![],
                vec![b"message::{\"key\":\"vm.execute\"}".to_vec()],
                vec![],
                1,
            ))
            .unwrap();
        assert_eq!(provider.finalized_epoch().unwrap(), Epoch::from_stored(1));
        assert_eq!(forwarded.lock().unwrap().len(), 1);
    }

    #[test]
    fn governance_packets_are_consumed_internally() {
        let provider = HashgraphConsensusProvider::new();
        let governance = provider.governance();
        let proxy = provider.proxy();
        let stake = br#"{"nodeId":"a","ownerId":"o","action":"bond","amount":5000,"nonce":1,"lockSeconds":0,"reason":"","timestamp":1}"#;
        proxy
            .submit_tx(
                b"stake::"
                    .as_slice()
                    .to_vec()
                    .iter()
                    .chain(stake)
                    .copied()
                    .collect::<Vec<_>>()
                    .as_slice(),
            )
            .unwrap();
        let stake_tx = proxy.submit_ch().recv().unwrap();
        proxy
            .commit_block(Block::new(0, 0, vec![], vec![], vec![stake_tx], vec![], 1))
            .unwrap();
        assert_eq!(governance.staking_snapshot().get("a"), Some(&5_000));
    }

    #[test]
    fn snapshot_restore_round_trip() {
        let provider = HashgraphConsensusProvider::new();
        provider.submit("settlement-2", DIGEST).unwrap();
        let snapshot = provider
            .proxy()
            .get_snapshot(0)
            .unwrap_or_else(|_| b"{}".to_vec());
        let restored = HashgraphConsensusProvider::new();
        restored.proxy().restore(&snapshot).unwrap();
        assert_eq!(provider.pending().unwrap(), restored.pending().unwrap());
    }
}
