//! One transaction per state action (ADR 0036).
//!
//! Every state action runs in one transaction on the storage provider plugin the
//! node loaded: it commits when the action succeeds and rolls back when the action
//! refuses. There is one provider, so there is no cross-provider ordering or
//! compensation (ADR 0026's dual routing is gone).

use anyhow::{Result, anyhow};

use crate::core::trx::Trx;

/// Why a state action did not take effect.
pub(crate) enum StateFailure {
    /// The action itself refused; its writes were discarded (LD-15).
    Action(anyhow::Error),
    /// The provider could not commit (LD-10).
    Storage(anyhow::Error),
}

impl StateFailure {
    pub(crate) fn into_error(self) -> anyhow::Error {
        match self {
            Self::Action(error) | Self::Storage(error) => error,
        }
    }
}

/// Run `action` in `trx`, then commit it, or roll it back when the action refuses.
/// A read-only transaction is closed without writing.
pub(crate) fn run_action(
    trx: &Trx,
    action: impl FnOnce() -> Result<()>,
) -> Result<(), StateFailure> {
    match action() {
        Ok(()) if trx.read_only() => {
            let _ = trx.rollback();
            Ok(())
        }
        Ok(()) => trx
            .commit()
            .map_err(|error| StateFailure::Storage(anyhow!("storage commit failed: {error}"))),
        Err(error) => {
            let _ = trx.rollback();
            Err(StateFailure::Action(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_storage::client::core::marker;
    use aseman_storage::{Mode, Models, Storage};

    fn storage() -> Storage {
        Storage::new(
            aseman_storage::memory::MemoryProvider::new(),
            aseman_storage::schema::Schema::catalog().unwrap(),
        )
    }

    fn put(trx: &Trx, key: &str) -> Result<()> {
        trx.marker()
            .create(marker::Create {
                key: key.to_owned(),
                value: "1".to_owned(),
            })
            .map(drop)
            .map_err(|error| anyhow!("{error}"))
    }

    #[test]
    fn an_action_commits_on_success_and_rolls_back_on_refusal() {
        let storage = storage();
        let trx = storage.begin(Mode::ReadWrite).unwrap();
        assert!(run_action(&trx, || put(&trx, "kept")).is_ok());
        let trx = storage.begin(Mode::ReadWrite).unwrap();
        assert!(matches!(
            run_action(&trx, || {
                put(&trx, "dropped")?;
                Err(anyhow!("refused"))
            }),
            Err(StateFailure::Action(_))
        ));
        let reader = storage.begin(Mode::ReadOnly).unwrap();
        assert!(
            reader
                .marker()
                .find_unique(marker::by_key("kept"))
                .unwrap()
                .is_some()
        );
        assert!(
            reader
                .marker()
                .find_unique(marker::by_key("dropped"))
                .unwrap()
                .is_none()
        );
        assert!(run_action(&reader, || Ok(())).is_ok());
    }
}
