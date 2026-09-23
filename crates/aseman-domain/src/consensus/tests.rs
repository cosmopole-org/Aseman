use super::*;

fn digest() -> String {
    format!("sha256:{}", "a".repeat(64))
}

fn checkpoint(epoch: u64) -> Checkpoint {
    Checkpoint {
        epoch: Epoch::from_stored(epoch),
        record_count: 100,
        digest: digest(),
        taken_at_millis: 1_000,
    }
}

fn finalized(epoch: u64, position: u64) -> Finalized {
    Finalized {
        idempotency_key: format!("key-{epoch}-{position}"),
        epoch: Epoch::from_stored(epoch),
        position,
        digest: digest(),
    }
}

#[test]
fn a_provider_changes_only_at_the_epoch_its_checkpoint_describes() {
    let epoch = Epoch::from_stored(7);
    may_switch(&checkpoint(7), epoch, 0).expect("a matching checkpoint");
    assert_eq!(
        may_switch(&checkpoint(6), epoch, 0),
        Err(ConsensusError::StaleCheckpoint),
        "a checkpoint of another moment does not describe what is being inherited"
    );
}

#[test]
fn records_in_flight_block_a_switch() {
    assert_eq!(
        may_switch(&checkpoint(7), Epoch::from_stored(7), 3),
        Err(ConsensusError::RecordsInFlight(3)),
        "they would be ordered by neither provider"
    );
}

#[test]
fn a_checkpoint_without_a_real_digest_cannot_be_inherited() {
    let mut broken = checkpoint(7);
    broken.digest = "sha256:short".to_owned();
    assert_eq!(
        may_switch(&broken, Epoch::from_stored(7), 0),
        Err(ConsensusError::InvalidDigest)
    );
}

#[test]
fn a_finalized_epoch_never_reopens() {
    let last = finalized(5, 10);
    assert!(accept_finalized(Some(&last), &finalized(5, 11)).is_ok());
    assert!(accept_finalized(Some(&last), &finalized(6, 0)).is_ok());
    assert_eq!(
        accept_finalized(Some(&last), &finalized(4, 999)),
        Err(ConsensusError::EpochWentBackwards)
    );
}

#[test]
fn a_finalized_order_only_ever_extends() {
    let last = finalized(5, 10);
    assert_eq!(
        accept_finalized(Some(&last), &finalized(5, 10)),
        Err(ConsensusError::OutOfOrder),
        "the ledger entries behind position 10 are already committed"
    );
    assert_eq!(
        accept_finalized(Some(&last), &finalized(5, 9)),
        Err(ConsensusError::OutOfOrder)
    );
    assert!(
        accept_finalized(None, &finalized(0, 0)).is_ok(),
        "a node with no history accepts the first finalization"
    );
}

#[test]
fn a_disagreement_is_reported_rather_than_repaired() {
    let record = finalized(1, 0);
    assert!(agrees(&record, &digest()));
    assert!(
        !agrees(&record, &format!("sha256:{}", "b".repeat(64))),
        "two nodes disagreeing about money is a thing a person must look at"
    );
}

#[test]
fn epochs_are_monotonic_and_bounded() {
    assert_eq!(Epoch::GENESIS.get(), 0);
    assert_eq!(Epoch::GENESIS.next().expect("next").get(), 1);
    assert_eq!(
        Epoch::from_stored(u64::MAX).next(),
        Err(ConsensusError::EpochOverflow),
        "wrapping would reuse an epoch that has already been finalized"
    );
}
