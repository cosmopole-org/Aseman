//! A804 finance-consensus port over the real Babble application proxy.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use anyhow::{Result as AnyResult, anyhow};
use aseman_domain::consensus::{Checkpoint, Epoch, Finalized};
use aseman_ports::consensus::ConsensusProvider;
use aseman_ports::{PortError, PortResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::hashgraph::Block;
use crate::node::state::State as NodeState;
use crate::proxy::{CommitResponse, InmemProxy, ProxyHandler};

const PROVIDER_NAME: &str = "hashgraph";

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
        self.finalized.last().map_or_else(
            || {
                self.adopted
                    .as_ref()
                    .map_or(Epoch::GENESIS, |item| item.epoch)
            },
            |item| item.epoch,
        )
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
            hash.update([b'\n']);
        }
        Checkpoint {
            epoch: self.epoch(),
            record_count: adopted_count.saturating_add(self.finalized.len() as u64),
            digest: format!("sha256:{}", hex::encode(hash.finalize())),
            taken_at_millis,
        }
    }
}

struct FinanceHandler {
    state: Arc<Mutex<ProviderState>>,
}

impl ProxyHandler for FinanceHandler {
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
        for (position, bytes) in transactions.iter().enumerate() {
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
                position: u64::try_from(position).map_err(|_| anyhow!("position overflow"))?,
                digest: record.digest,
            });
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

/// Finance ordering backed by the same proxy consumed by a [`crate::babble::Babble`] engine.
pub struct HashgraphConsensusProvider {
    state: Arc<Mutex<ProviderState>>,
    proxy: Arc<InmemProxy>,
}

impl HashgraphConsensusProvider {
    #[must_use]
    pub fn new() -> Self {
        let state = Arc::new(Mutex::new(ProviderState::default()));
        let handler: Arc<dyn ProxyHandler> = Arc::new(FinanceHandler {
            state: Arc::clone(&state),
        });
        Self {
            state,
            proxy: Arc::new(InmemProxy::new(handler, None)),
        }
    }

    /// The application proxy that must be installed in the Babble configuration.
    #[must_use]
    pub fn proxy(&self) -> Arc<InmemProxy> {
        Arc::clone(&self.proxy)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashgraph::Block;
    use crate::proxy::AppProxy;

    const DIGEST: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn submitted_records_are_finalized_only_by_a_hashgraph_commit() {
        let provider = HashgraphConsensusProvider::new();
        provider.submit("settlement-1", DIGEST).unwrap();
        provider.submit("settlement-1", DIGEST).unwrap();
        assert_eq!(provider.pending().unwrap(), 1);
        let proxy = provider.proxy();
        let transaction = proxy.submit_ch().recv().unwrap();
        proxy
            .commit_block(Block::new(
                0,
                0,
                vec![],
                vec![],
                vec![transaction],
                vec![],
                42,
            ))
            .unwrap();
        assert_eq!(provider.pending().unwrap(), 0);
        assert_eq!(provider.finalized_epoch().unwrap(), Epoch::from_stored(1));
        let records = provider.finalized_after(Epoch::GENESIS, 10).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].idempotency_key, "settlement-1");
    }

    #[test]
    fn handoff_refuses_in_flight_or_existing_history() {
        let provider = HashgraphConsensusProvider::new();
        provider.submit("settlement-1", DIGEST).unwrap();
        let checkpoint = Checkpoint {
            epoch: Epoch::from_stored(7),
            record_count: 20,
            digest: DIGEST.to_owned(),
            taken_at_millis: 10,
        };
        assert_eq!(provider.adopt(&checkpoint), Err(PortError::Conflict));

        let empty = HashgraphConsensusProvider::new();
        empty.adopt(&checkpoint).unwrap();
        assert_eq!(empty.finalized_epoch().unwrap(), Epoch::from_stored(7));
        assert_eq!(empty.adopt(&checkpoint), Err(PortError::Conflict));
    }
}
