use super::*;

fn workload() -> Uuid {
    Uuid::from_bytes([3; 16])
}

fn sample(id: &str, at: i64, cpu: u64, egress: u64) -> UsageSample {
    UsageSample {
        workload_id: workload(),
        provider_sample_id: id.to_owned(),
        provider: "nomad".to_owned(),
        collected_at_millis: at,
        cumulative: BTreeMap::from([
            (Dimension::CpuMillis, cpu),
            (Dimension::NetworkEgressBytes, egress),
        ]),
    }
}

fn prices() -> PriceList {
    PriceList {
        version: "2026-09".to_owned(),
        effective_from_millis: 0,
        rates: BTreeMap::from([
            // One minor unit per second of CPU.
            (Dimension::CpuMillis, PRICE_SCALE / 1_000),
            // One minor unit per mebibyte of egress.
            (Dimension::NetworkEgressBytes, PRICE_SCALE / (1024 * 1024)),
        ]),
    }
}

#[test]
fn a_price_list_with_a_rate_that_rounds_to_zero_is_refused() {
    // The mistake this catches: writing a per-byte price at too coarse a scale, so
    // the rate truncates to zero and the dimension is silently free.
    let zero = PriceList {
        version: "broken".to_owned(),
        effective_from_millis: 0,
        rates: BTreeMap::from([(Dimension::NetworkEgressBytes, 0)]),
    };
    assert_eq!(
        zero.validate(),
        Err(FinanceError::ZeroRate(Dimension::NetworkEgressBytes))
    );
    let unbillable = PriceList {
        version: "broken".to_owned(),
        effective_from_millis: 0,
        rates: BTreeMap::from([(Dimension::NetworkIngressBytes, 5)]),
    };
    assert_eq!(
        unbillable.validate(),
        Err(FinanceError::UnbillableDimension(
            Dimension::NetworkIngressBytes
        ))
    );
    prices()
        .validate()
        .expect("the real price list is expressible");
}

#[test]
fn a_settlement_identity_is_the_workload_the_interval_and_the_sample() {
    let interval = interval_between(&sample("s1", 0, 0, 0), &sample("s2", 60_000, 1_000, 0))
        .expect("an interval");
    assert_eq!(
        interval.settlement_key(),
        format!("{}:0:s2", workload()),
        "the same reading collected twice settles once"
    );
}

#[test]
fn cumulative_counters_become_interval_deltas() {
    let interval = interval_between(
        &sample("s1", 0, 5_000, 1_000),
        &sample("s2", 60_000, 8_000, 3_000),
    )
    .expect("an interval");
    assert_eq!(interval.deltas[&Dimension::CpuMillis], 3_000);
    assert_eq!(interval.deltas[&Dimension::NetworkEgressBytes], 2_000);
    assert_eq!(interval.interval_start_millis, 0);
    assert_eq!(interval.interval_end_millis, 60_000);
}

#[test]
fn a_counter_that_reset_is_read_as_the_new_counter_not_a_huge_delta() {
    // The workload restarted, so its counters went back to near zero. Subtracting
    // would underflow; treating that as unsigned would be an enormous charge.
    let interval = interval_between(
        &sample("s1", 0, 900_000, 0),
        &sample("s2", 60_000, 1_200, 0),
    )
    .expect("an interval");
    assert_eq!(
        interval.deltas[&Dimension::CpuMillis],
        1_200,
        "a reset counter bills what the new counter says, not what the subtraction says"
    );
}

#[test]
fn samples_must_be_the_same_workloads_and_in_order() {
    let mut other = sample("s2", 60_000, 1, 0);
    other.workload_id = Uuid::from_bytes([9; 16]);
    assert_eq!(
        interval_between(&sample("s1", 0, 0, 0), &other),
        Err(FinanceError::DifferentWorkloads)
    );
    assert_eq!(
        interval_between(&sample("s1", 60_000, 0, 0), &sample("s2", 60_000, 1, 0)),
        Err(FinanceError::OutOfOrder),
        "two readings at the same instant are not an interval"
    );
}

#[test]
fn pricing_is_deterministic_and_explains_itself() {
    let interval = interval_between(
        &sample("s1", 0, 0, 0),
        &sample("s2", 60_000, 30_000, 10 * 1024 * 1024),
    )
    .expect("an interval");
    let charge = price(&interval, &prices()).expect("a charge");
    assert_eq!(charge.lines[&Dimension::CpuMillis], Minor(30));
    assert_eq!(charge.lines[&Dimension::NetworkEgressBytes], Minor(10));
    assert_eq!(charge.amount, Minor(40));
    assert_eq!(charge.price_version, "2026-09");
    assert_eq!(
        price(&interval, &prices()).expect("again"),
        charge,
        "the same interval and price list always produce the same charge"
    );
}

#[test]
fn ingress_is_never_billable() {
    assert!(!Dimension::NetworkIngressBytes.billable());
    let mut interval =
        interval_between(&sample("s1", 0, 0, 0), &sample("s2", 60_000, 0, 0)).expect("an interval");
    interval
        .deltas
        .insert(Dimension::NetworkIngressBytes, 1_000_000_000);
    // A price list that does not price ingress still prices this interval: charging
    // for ingress would let anyone on the internet spend a creature's balance.
    let charge = price(&interval, &prices()).expect("a charge");
    assert_eq!(charge.amount, Minor(0));
}

#[test]
fn a_billable_dimension_the_price_list_forgot_is_an_error_not_a_free_ride() {
    let mut interval =
        interval_between(&sample("s1", 0, 0, 0), &sample("s2", 60_000, 0, 0)).expect("an interval");
    interval.deltas.insert(Dimension::AcceleratorMillis, 500);
    assert_eq!(
        price(&interval, &prices()),
        Err(FinanceError::UnpricedDimension(
            Dimension::AcceleratorMillis
        )),
        "silently charging zero would be a revenue hole nobody notices"
    );
}

#[test]
fn a_fraction_of_a_minor_unit_consumed_is_a_minor_unit_owed() {
    let interval =
        interval_between(&sample("s1", 0, 0, 0), &sample("s2", 60_000, 1, 0)).expect("an interval");
    let charge = price(&interval, &prices()).expect("a charge");
    assert_eq!(
        charge.amount,
        Minor(1),
        "rounding down would make a busy workload free in the small"
    );
}

#[test]
fn the_price_list_in_force_is_the_newest_one_that_had_taken_effect() {
    let old = PriceList {
        version: "old".to_owned(),
        effective_from_millis: 0,
        rates: BTreeMap::new(),
    };
    let new = PriceList {
        version: "new".to_owned(),
        effective_from_millis: 1_000,
        rates: BTreeMap::new(),
    };
    let lists = [old, new];
    assert_eq!(price_list_at(&lists, 999).unwrap().version, "old");
    assert_eq!(price_list_at(&lists, 1_000).unwrap().version, "new");
    assert!(
        price_list_at(&lists[1..], -1).is_none(),
        "an interval before any price list has no price"
    );
}

#[test]
fn a_settlement_balances_and_traces_to_its_price() {
    let interval = interval_between(&sample("s1", 0, 0, 0), &sample("s2", 60_000, 30_000, 0))
        .expect("an interval");
    let charge = price(&interval, &prices()).expect("a charge");
    let record = settle(&charge, "wallet:alice", "revenue:compute", 60_000).expect("a record");
    assert!(record.balances());
    assert_eq!(record.idempotency_key, charge.settlement_key);
    assert_eq!(record.price_version.as_deref(), Some("2026-09"));
    assert_eq!(record.entries[0].amount, Minor(-30));
    assert_eq!(record.entries[1].amount, Minor(30));
}

#[test]
fn an_unbalanced_record_is_not_a_record() {
    let record = JournalRecord {
        idempotency_key: "k".to_owned(),
        at_millis: 0,
        entries: vec![
            Entry {
                account: "wallet:alice".to_owned(),
                amount: Minor(-30),
            },
            Entry {
                account: "revenue:compute".to_owned(),
                amount: Minor(29),
            },
        ],
        price_version: None,
    };
    assert!(!record.balances(), "a penny short is not a journal record");
}

#[test]
fn money_never_wraps() {
    assert_eq!(
        Minor(i64::MAX).checked_add(Minor(1)),
        Err(FinanceError::Overflow)
    );
    assert_eq!(
        Minor(i64::MIN).checked_sub(Minor(1)),
        Err(FinanceError::Overflow)
    );
}

#[test]
fn enforcement_escalates_and_never_jumps_to_stopping() {
    let policy = EnforcementPolicy {
        grace_millis: 60 * 60 * 1000,
        pause_millis: 24 * 60 * 60 * 1000,
    };
    assert_eq!(enforcement(Minor(10), 0, policy), Enforcement::None);
    assert_eq!(
        enforcement(Minor(0), i64::MAX, policy),
        Enforcement::None,
        "a zero balance is paid up"
    );
    assert_eq!(
        enforcement(Minor(-5), 0, policy),
        Enforcement::Notify,
        "a balance that dips between a charge and a top-up is normal"
    );
    assert_eq!(
        enforcement(Minor(-5), policy.grace_millis - 1, policy),
        Enforcement::Notify
    );
    assert_eq!(
        enforcement(Minor(-5), policy.grace_millis, policy),
        Enforcement::Pause,
        "pausing preserves memory, so paying resumes rather than restarts"
    );
    assert_eq!(
        enforcement(
            Minor(-5),
            policy.grace_millis + policy.pause_millis - 1,
            policy
        ),
        Enforcement::Pause
    );
    assert_eq!(
        enforcement(Minor(-5), policy.grace_millis + policy.pause_millis, policy),
        Enforcement::Stop
    );
    assert!(
        Enforcement::Notify < Enforcement::Pause && Enforcement::Pause < Enforcement::Stop,
        "the steps are ordered, so a caller cannot skip one by comparison"
    );
}

fn hold() -> Hold {
    Hold {
        idempotency_key: "run-7".to_owned(),
        account: "wallet:alice".to_owned(),
        amount: Minor(100),
        state: HoldState::Held,
    }
}

#[test]
fn a_reservation_is_a_ceiling_not_a_suggestion() {
    let hold = hold();
    let record = capture(&hold, Minor(40), "revenue:compute", 1_000).expect("a capture");
    assert!(record.balances());
    assert_eq!(record.entries[0].amount, Minor(-40));
    assert_eq!(record.idempotency_key, "capture:run-7");
    assert_eq!(
        capture(&hold, Minor(101), "revenue:compute", 1_000),
        Err(FinanceError::CaptureExceedsHold),
        "capturing more than was held would spend money nobody set aside"
    );
}

#[test]
fn a_finished_hold_cannot_be_captured_again() {
    for state in [HoldState::Captured, HoldState::Released] {
        let finished = Hold { state, ..hold() };
        assert!(!finished.open());
        assert_eq!(
            capture(&finished, Minor(1), "revenue:compute", 1_000),
            Err(FinanceError::HoldClosed),
            "capturing twice is what holds exist to prevent"
        );
    }
}

#[test]
fn a_refund_is_a_new_record_never_an_edit() {
    let interval = interval_between(&sample("s1", 0, 0, 0), &sample("s2", 60_000, 30_000, 0))
        .expect("an interval");
    let charge = price(&interval, &prices()).expect("a charge");
    let settled = settle(&charge, "wallet:alice", "revenue:compute", 60_000).expect("settled");

    let refunded = refund(&settled, "goodwill", 120_000).expect("a refund");
    assert!(refunded.balances());
    assert_ne!(
        refunded.idempotency_key, settled.idempotency_key,
        "the journal is append-only: the original stands"
    );
    assert_eq!(
        refunded.entries[0].amount,
        Minor(30),
        "the wallet gets it back"
    );
    assert_eq!(refunded.entries[1].amount, Minor(-30));
    assert_eq!(
        refunded.price_version, settled.price_version,
        "a refund traces to the same price as the charge it reverses"
    );
}

#[test]
fn reconciliation_proposes_a_correction_only_where_arithmetic_can_settle_it() {
    // A misprice has a defensible correction: the difference.
    let mispriced = Discrepancy::Mispriced {
        settlement_key: "w:0:s1".to_owned(),
        settled: Minor(30),
        recomputed: Minor(45),
    };
    let record = corrective_entry(&mispriced, "wallet:alice", "revenue:compute", 1_000)
        .expect("a proposal")
        .expect("some");
    assert!(record.balances());
    assert_eq!(record.entries[0].amount, Minor(-15));
    assert_eq!(record.idempotency_key, "correction:w:0:s1");

    // Nothing to correct.
    let equal = Discrepancy::Mispriced {
        settlement_key: "w:0:s1".to_owned(),
        settled: Minor(30),
        recomputed: Minor(30),
    };
    assert!(
        corrective_entry(&equal, "wallet:alice", "revenue:compute", 1_000)
            .expect("no proposal")
            .is_none()
    );

    // The rest are questions for a person, not arithmetic.
    for discrepancy in [
        Discrepancy::Unsettled {
            settlement_key: "w:0:s2".to_owned(),
        },
        Discrepancy::Unexplained {
            idempotency_key: "mystery".to_owned(),
        },
        Discrepancy::UnknownPrice {
            idempotency_key: "old".to_owned(),
            price_version: "gone".to_owned(),
        },
    ] {
        assert!(
            corrective_entry(&discrepancy, "wallet:alice", "revenue:compute", 1_000)
                .expect("no proposal")
                .is_none(),
            "a reconciler that silently moves money turns one bad charge into a series"
        );
    }
}
