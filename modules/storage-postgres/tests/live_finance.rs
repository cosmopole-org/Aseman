//! The Phase 8 gate, live: crash and retry produce neither duplicate nor missing
//! settled intervals, and every charge traces to a raw usage sample and a price
//! version.

use std::collections::BTreeMap;
use std::str::FromStr;

use aseman_domain::Uuid;
use aseman_domain::finance::{
    Dimension, Minor, PriceList, UsageSample, interval_between, price, price_list_at, settle,
};
use aseman_ports::PortError;
use aseman_ports::finance::{Ledger, PricingStore, UsageStore};
use aseman_storage_postgres::finance::PostgresFinance;
use postgres::{Client, Config, NoTls};

const WALLET: &str = "wallet:alice";
const REVENUE: &str = "revenue:compute";

fn sample(workload: Uuid, id: &str, at: i64, cpu: u64) -> UsageSample {
    UsageSample {
        workload_id: workload,
        provider_sample_id: id.to_owned(),
        provider: "nomad".to_owned(),
        collected_at_millis: at,
        cumulative: BTreeMap::from([(Dimension::CpuMillis, cpu)]),
    }
}

fn prices() -> PriceList {
    PriceList {
        version: "2026-09".to_owned(),
        effective_from_millis: 0,
        // One minor unit per second of CPU.
        rates: BTreeMap::from([(Dimension::CpuMillis, 1_000_000)]),
    }
}

#[test]
fn live_settlement_never_duplicates_and_never_loses_an_interval() {
    let Some(admin_uri) = aseman_config::IntegrationTestConfig::from_process().postgres_url else {
        eprintln!("ASEMAN_TEST_POSTGRES_URL is absent; skipping finance test");
        return;
    };
    let database = format!("aseman_finance_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);

    let finance = PostgresFinance::connect_config(config, 8).unwrap();
    finance.migrate().unwrap();
    finance.migrate().unwrap();

    finance.publish(&prices()).unwrap();
    // A published price is never edited: charges refer to it by version.
    assert_eq!(finance.publish(&prices()), Err(PortError::Conflict));
    // A price nobody could be charged at is refused at publication.
    let broken = PriceList {
        version: "broken".to_owned(),
        effective_from_millis: 0,
        rates: BTreeMap::from([(Dimension::NetworkEgressBytes, 0)]),
    };
    assert!(matches!(
        finance.publish(&broken),
        Err(PortError::Denied(_))
    ));

    let workload = Uuid::now_v7();

    // Collect a minute of readings. The first reading is a baseline: there is no
    // interval before it, and nothing is charged for it.
    let first = sample(workload, "s-1", 0, 0);
    finance.record_sample(&first).unwrap();
    assert!(finance.previous_sample(workload, 0).unwrap().is_none());

    // Collecting the same provider reading twice is refused, so it can never become a
    // second interval.
    assert_eq!(finance.record_sample(&first), Err(PortError::Conflict));

    let mut charged = Minor(0);
    for minute in 1..=5_i64 {
        let at = minute * 60_000;
        let current = sample(
            workload,
            &format!("s-{minute}x"),
            at,
            (minute as u64) * 30_000,
        );
        finance.record_sample(&current).unwrap();
        let previous = finance
            .previous_sample(workload, at)
            .unwrap()
            .expect("a baseline");
        let interval = interval_between(&previous, &current).unwrap();
        finance.record_interval(&interval).unwrap();

        let lists = finance.price_lists().unwrap();
        let list = price_list_at(&lists, interval.interval_start_millis).expect("a price");
        let charge = price(&interval, list).unwrap();
        let record = settle(&charge, WALLET, REVENUE, interval.interval_end_millis).unwrap();

        // The crash: the same settlement is committed several times, as it would be
        // by a retry after a process died between committing and recording that it
        // had. Every extra commit must change nothing.
        finance.commit(&record).unwrap();
        finance.commit(&record).unwrap();
        finance.commit(&record).unwrap();
        charged = charged.checked_add(charge.amount).unwrap();
    }

    // Neither duplicated nor missing: the balance is exactly the five intervals.
    assert_eq!(
        finance.balance(WALLET).unwrap(),
        Minor(-charged.0),
        "the wallet is charged once per interval"
    );
    assert_eq!(finance.balance(REVENUE).unwrap(), charged);
    assert_eq!(
        charged,
        Minor(150),
        "five minutes at 30 seconds of CPU each"
    );

    // Nothing is left unsettled.
    assert!(
        finance.unsettled(100).unwrap().is_empty(),
        "every interval has a journal record"
    );

    // Every charge traces to a sample and a price version.
    let settlements = finance.settlements(workload, 100).unwrap();
    assert_eq!(settlements.len(), 5);
    for record in &settlements {
        assert!(record.balances());
        assert_eq!(record.price_version.as_deref(), Some("2026-09"));
        // The idempotency key is the settlement identity, so the raw sample it came
        // from is named in the key itself.
        assert!(
            record.idempotency_key.starts_with(&workload.to_string()),
            "{}",
            record.idempotency_key
        );
    }

    // An outage: a whole minute is collected late, out of order. It settles exactly
    // once, and the ones around it are undisturbed.
    let late = sample(workload, "s-late", 6 * 60_000, 6 * 30_000);
    finance.record_sample(&late).unwrap();
    let previous = finance
        .previous_sample(workload, 6 * 60_000)
        .unwrap()
        .unwrap();
    let interval = interval_between(&previous, &late).unwrap();
    finance.record_interval(&interval).unwrap();
    // A duplicated interval is refused outright.
    assert_eq!(finance.record_interval(&interval), Err(PortError::Conflict));
    let lists = finance.price_lists().unwrap();
    let list = price_list_at(&lists, interval.interval_start_millis).unwrap();
    let charge = price(&interval, list).unwrap();
    finance
        .commit(&settle(&charge, WALLET, REVENUE, interval.interval_end_millis).unwrap())
        .unwrap();
    assert_eq!(finance.balance(WALLET).unwrap(), Minor(-180));
    assert!(finance.unsettled(100).unwrap().is_empty());

    // A record that does not balance is not committed at all.
    let mut unbalanced = settle(&charge, WALLET, REVENUE, interval.interval_end_millis).unwrap();
    unbalanced.idempotency_key = "hand-written".to_owned();
    unbalanced.entries[1].amount = Minor(unbalanced.entries[1].amount.0 + 1);
    assert!(matches!(
        finance.commit(&unbalanced),
        Err(PortError::Denied(_))
    ));
    assert!(finance.record("hand-written").unwrap().is_none());
    assert_eq!(
        finance.balance(WALLET).unwrap(),
        Minor(-180),
        "a refused record leaves no half-entry behind"
    );

    drop(finance);
    admin
        .batch_execute(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .unwrap();
}
