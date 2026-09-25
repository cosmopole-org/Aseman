//! A703 listener bind, atomic handoff, bounded drain, failure, and rollback semantics.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListenerPhase {
    Staged,
    Active,
    Draining,
    Retired,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListenerBinding {
    pub generation: u64,
    pub endpoint: String,
    pub phase: ListenerPhase,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListenerBroker {
    active: Option<ListenerBinding>,
    staged: Option<ListenerBinding>,
    draining: Option<ListenerBinding>,
    retired: Vec<ListenerBinding>,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ListenerTransitionError {
    #[error("the listener endpoint is empty")]
    EmptyEndpoint,
    #[error("listener generation must be positive and newer than every known generation")]
    StaleGeneration,
    #[error("a different listener generation is already staged")]
    CandidateExists,
    #[error("listener generation {0} is not staged")]
    NotStaged(u64),
    #[error("listener generation {0} is not active")]
    NotActive(u64),
    #[error("listener generation {0} is not draining")]
    NotDraining(u64),
    #[error("the previous listener is still draining")]
    DrainInProgress,
}

impl ListenerBroker {
    #[must_use]
    pub const fn active(&self) -> Option<&ListenerBinding> {
        self.active.as_ref()
    }

    #[must_use]
    pub const fn staged(&self) -> Option<&ListenerBinding> {
        self.staged.as_ref()
    }

    #[must_use]
    pub const fn draining(&self) -> Option<&ListenerBinding> {
        self.draining.as_ref()
    }

    #[must_use]
    pub fn retired(&self) -> &[ListenerBinding] {
        &self.retired
    }

    /// Stage a bound candidate. Repeating the same request is idempotent.
    pub fn stage(
        &mut self,
        generation: u64,
        endpoint: impl Into<String>,
    ) -> Result<(), ListenerTransitionError> {
        let endpoint = endpoint.into();
        if endpoint.trim().is_empty() {
            return Err(ListenerTransitionError::EmptyEndpoint);
        }
        if self
            .staged
            .as_ref()
            .is_some_and(|binding| binding.generation == generation && binding.endpoint == endpoint)
        {
            return Ok(());
        }
        if self.staged.is_some() {
            return Err(ListenerTransitionError::CandidateExists);
        }
        let newest = self
            .active
            .iter()
            .chain(self.draining.iter())
            .chain(self.retired.iter())
            .map(|binding| binding.generation)
            .max()
            .unwrap_or(0);
        if generation == 0 || generation <= newest {
            return Err(ListenerTransitionError::StaleGeneration);
        }
        self.staged = Some(ListenerBinding {
            generation,
            endpoint,
            phase: ListenerPhase::Staged,
        });
        Ok(())
    }

    /// Record a candidate bind/readiness failure without disturbing active traffic.
    pub fn fail_staged(&mut self, generation: u64) -> Result<(), ListenerTransitionError> {
        let Some(mut failed) = self.staged.take() else {
            return Err(ListenerTransitionError::NotStaged(generation));
        };
        if failed.generation != generation {
            self.staged = Some(failed);
            return Err(ListenerTransitionError::NotStaged(generation));
        }
        failed.phase = ListenerPhase::Failed;
        self.retired.push(failed);
        Ok(())
    }

    /// Atomically direct new connections to a ready candidate.
    pub fn activate(&mut self, generation: u64) -> Result<(), ListenerTransitionError> {
        if self
            .active
            .as_ref()
            .is_some_and(|binding| binding.generation == generation)
        {
            return Ok(());
        }
        if self.draining.is_some() {
            return Err(ListenerTransitionError::DrainInProgress);
        }
        let Some(mut candidate) = self.staged.take() else {
            return Err(ListenerTransitionError::NotStaged(generation));
        };
        if candidate.generation != generation {
            self.staged = Some(candidate);
            return Err(ListenerTransitionError::NotStaged(generation));
        }
        if let Some(mut previous) = self.active.take() {
            previous.phase = ListenerPhase::Draining;
            self.draining = Some(previous);
        }
        candidate.phase = ListenerPhase::Active;
        self.active = Some(candidate);
        Ok(())
    }

    /// Finish a successful or timed-out bounded drain.
    pub fn finish_drain(&mut self, generation: u64) -> Result<(), ListenerTransitionError> {
        let Some(mut drained) = self.draining.take() else {
            return Err(ListenerTransitionError::NotDraining(generation));
        };
        if drained.generation != generation {
            self.draining = Some(drained);
            return Err(ListenerTransitionError::NotDraining(generation));
        }
        drained.phase = ListenerPhase::Retired;
        self.retired.push(drained);
        Ok(())
    }

    /// Roll back a failed active generation to the still-draining predecessor.
    pub fn rollback(&mut self, generation: u64) -> Result<(), ListenerTransitionError> {
        let Some(mut failed) = self.active.take() else {
            return Err(ListenerTransitionError::NotActive(generation));
        };
        if failed.generation != generation {
            self.active = Some(failed);
            return Err(ListenerTransitionError::NotActive(generation));
        }
        failed.phase = ListenerPhase::Failed;
        self.retired.push(failed);
        if let Some(mut previous) = self.draining.take() {
            previous.phase = ListenerPhase::Active;
            self.active = Some(previous);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_handoff_has_one_active_and_drains_the_predecessor() {
        let mut broker = ListenerBroker::default();
        broker.stage(1, "127.0.0.1:443").unwrap();
        broker.activate(1).unwrap();
        broker.stage(2, "127.0.0.1:8443").unwrap();
        broker.activate(2).unwrap();
        assert_eq!(broker.active().unwrap().generation, 2);
        assert_eq!(broker.draining().unwrap().generation, 1);
        broker.finish_drain(1).unwrap();
        assert!(broker.draining().is_none());
        assert_eq!(broker.retired()[0].phase, ListenerPhase::Retired);
    }

    #[test]
    fn candidate_failure_never_disturbs_the_active_listener() {
        let mut broker = ListenerBroker::default();
        broker.stage(1, "api:443").unwrap();
        broker.activate(1).unwrap();
        broker.stage(2, "api:8443").unwrap();
        broker.fail_staged(2).unwrap();
        assert_eq!(broker.active().unwrap().generation, 1);
        assert_eq!(broker.retired()[0].phase, ListenerPhase::Failed);
    }

    #[test]
    fn post_handoff_failure_restores_the_draining_generation() {
        let mut broker = ListenerBroker::default();
        broker.stage(1, "api:443").unwrap();
        broker.activate(1).unwrap();
        broker.stage(2, "api:8443").unwrap();
        broker.activate(2).unwrap();
        broker.rollback(2).unwrap();
        assert_eq!(broker.active().unwrap().generation, 1);
        assert!(broker.draining().is_none());
        assert_eq!(broker.retired()[0].generation, 2);
        assert_eq!(broker.retired()[0].phase, ListenerPhase::Failed);
    }

    #[test]
    fn generations_are_monotonic_and_transitions_are_generation_bound() {
        let mut broker = ListenerBroker::default();
        assert_eq!(
            broker.stage(0, "api:443"),
            Err(ListenerTransitionError::StaleGeneration)
        );
        broker.stage(4, "api:443").unwrap();
        assert_eq!(
            broker.activate(3),
            Err(ListenerTransitionError::NotStaged(3))
        );
        broker.activate(4).unwrap();
        assert_eq!(
            broker.stage(4, "api:8443"),
            Err(ListenerTransitionError::StaleGeneration)
        );
    }
}
