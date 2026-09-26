//! Finance ordering through the consensus port (Phase 8, RL-011).
//!
//! Finance decides wallets, pricing, and the ledger without consensus. What consensus
//! adds is an agreed **order** past which records will not change — the epoch boundary
//! a provider handover needs. The use cases here submit the finance journal records the
//! settled flows produce, so their order is provable across nodes.

use aseman_domain::consensus::{Checkpoint, Epoch};
use aseman_ports::PortError;
use aseman_ports::consensus::ConsensusProvider;
use sha2::{Digest, Sha256};

use crate::ApplicationError;

/// The idempotency key a finance journal record is ordered under.
#[must_use]
pub fn journal_idempotency_key(journal_id: &str) -> String {
    format!("journal:{journal_id}")
}

/// The `sha256:{hex}` digest of a journal record's idempotency key, as the provider
/// orders records by digest.
#[must_use]
pub fn journal_digest(journal_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(journal_idempotency_key(journal_id).as_bytes());
    format!("sha256:{}", hex::encode(hasher.finalize()))
}

/// Offer one finance journal record for ordering. Submitting the same record twice is
/// success: the record is already in the order, or on its way into it.
pub struct SubmitFinanceRecord<'a> {
    pub consensus: &'a dyn ConsensusProvider,
}

impl SubmitFinanceRecord<'_> {
    /// # Errors
    ///
    /// When the provider is unreachable, or when the record was already ordered under
    /// another digest (which would mean two journals share an idempotency key).
    pub fn execute(&self, journal_id: &str) -> Result<(), ApplicationError> {
        self.consensus
            .submit(
                &journal_idempotency_key(journal_id),
                &journal_digest(journal_id),
            )
            .map_err(ApplicationError::from)
    }
}

/// The finance flow's view of where the order stands, for a provider checkpoint.
pub struct FinanceConsensusStatus<'a> {
    pub consensus: &'a dyn ConsensusProvider,
}

impl FinanceConsensusStatus<'_> {
    /// The last finalized epoch.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable.
    pub fn finalized_epoch(&self) -> Result<Epoch, ApplicationError> {
        self.consensus
            .finalized_epoch()
            .map_err(ApplicationError::from)
    }

    /// A checkpoint of the order for a provider handover.
    ///
    /// # Errors
    ///
    /// When the provider is unreachable.
    pub fn checkpoint(&self, at_millis: i64) -> Result<Checkpoint, ApplicationError> {
        self.consensus
            .checkpoint(at_millis)
            .map_err(ApplicationError::from)
    }
}

/// The finance flow's readiness to hand ordering to another provider: nothing in
/// flight, and a checkpoint of the current order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandoverReadiness {
    pub pending: u64,
    pub checkpoint: Checkpoint,
}

/// Inspect the provider before a handover.
pub struct CheckHandoverReadiness<'a> {
    pub consensus: &'a dyn ConsensusProvider,
}

impl CheckHandoverReadiness<'_> {
    /// # Errors
    ///
    /// When the provider is unreachable, or when a checkpoint cannot be taken.
    pub fn execute(&self, at_millis: i64) -> Result<HandoverReadiness, ApplicationError> {
        let pending = self.consensus.pending().map_err(ApplicationError::Port)?;
        let checkpoint = self
            .consensus
            .checkpoint(at_millis)
            .map_err(ApplicationError::from)?;
        Ok(HandoverReadiness {
            pending,
            checkpoint,
        })
    }

    /// Whether the checkpoint's epoch is the outgoing one and nothing is in flight.
    pub fn may_switch(
        &self,
        readiness: &HandoverReadiness,
        outgoing_epoch: Epoch,
    ) -> Result<(), String> {
        aseman_domain::consensus::may_switch(
            &readiness.checkpoint,
            outgoing_epoch,
            readiness.pending,
        )
        .map_err(|error| error.to_string())
    }
}

/// Adopt a checkpoint as a provider's starting history (rollback to a prior order).
pub struct AdoptConsensusCheckpoint<'a> {
    pub consensus: &'a dyn ConsensusProvider,
}

impl AdoptConsensusCheckpoint<'_> {
    /// # Errors
    ///
    /// [`PortError::Conflict`] when the provider has already finalized something of its
    /// own — adopting then would fork the order.
    pub fn execute(&self, checkpoint: &Checkpoint) -> Result<(), ApplicationError> {
        match self.consensus.adopt(checkpoint) {
            Err(PortError::Conflict) => Err(ApplicationError::Denied(
                "the consensus provider already has history; it cannot adopt a checkpoint"
                    .to_owned(),
            )),
            other => other.map_err(ApplicationError::from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_ports::PortResult;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Memory {
        submitted: Mutex<Vec<(String, String)>>,
        adopted: Mutex<Option<Checkpoint>>,
    }

    impl ConsensusProvider for Memory {
        fn name(&self) -> &str {
            "memory"
        }
        fn submit(&self, idempotency_key: &str, digest: &str) -> PortResult<()> {
            self.submitted
                .lock()
                .unwrap()
                .push((idempotency_key.to_owned(), digest.to_owned()));
            Ok(())
        }
        fn finalized_epoch(&self) -> PortResult<Epoch> {
            Ok(Epoch::GENESIS)
        }
        fn finalized_after(
            &self,
            _: Epoch,
            _: usize,
        ) -> PortResult<Vec<aseman_domain::consensus::Finalized>> {
            Ok(Vec::new())
        }
        fn pending(&self) -> PortResult<u64> {
            Ok(0)
        }
        fn checkpoint(&self, at_millis: i64) -> PortResult<Checkpoint> {
            Ok(Checkpoint {
                epoch: Epoch::GENESIS,
                record_count: self.submitted.lock().unwrap().len() as u64,
                digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_owned(),
                taken_at_millis: at_millis,
            })
        }
        fn adopt(&self, checkpoint: &Checkpoint) -> PortResult<()> {
            let mut adopted = self.adopted.lock().unwrap();
            if adopted.is_some() {
                return Err(PortError::Conflict);
            }
            *adopted = Some(checkpoint.clone());
            Ok(())
        }
        fn set(&self, _key: &str, _value: &str) -> PortResult<()> {
            Err(PortError::Unsupported("memory consensus property"))
        }
    }

    #[test]
    fn a_finance_journal_is_offered_under_a_stable_key_and_digest() {
        let memory = Memory::default();
        SubmitFinanceRecord { consensus: &memory }
            .execute("journal-1")
            .unwrap();
        let submitted = memory.submitted.lock().unwrap();
        assert_eq!(submitted[0].0, "journal:journal-1");
        assert!(submitted[0].1.starts_with("sha256:"));
        assert_eq!(submitted[0].1.len(), "sha256:".len() + 64);
    }

    #[test]
    fn readiness_and_adoption_follow_the_domain_rules() {
        let memory = Memory::default();
        let readiness = CheckHandoverReadiness { consensus: &memory }
            .execute(10)
            .unwrap();
        assert_eq!(readiness.pending, 0);
        assert!(readiness.checkpoint.epoch == Epoch::GENESIS);
        // Adoption into an empty provider succeeds; into one that adopted refuses.
        AdoptConsensusCheckpoint { consensus: &memory }
            .execute(&readiness.checkpoint)
            .unwrap();
        assert!(matches!(
            AdoptConsensusCheckpoint { consensus: &memory }.execute(&readiness.checkpoint),
            Err(ApplicationError::Denied(_))
        ));
    }
}
