//! A1002 settlement soak: many intervals, repeated commits, one balanced result.

use std::collections::BTreeMap;
use std::str::FromStr;

use aseman_domain::Uuid;
use aseman_domain::finance::{Dimension, PriceList, UsageSample, interval_between, price, settle};
use aseman_finance_ledger::PostgresFinance;
use aseman_ports::finance::{Ledger, PricingStore, UsageStore};
use postgres::{Client, Config, NoTls};

const INTERVALS: i64 = 1_000;

fn sample(workload: Uuid, sequence: i64) -> UsageSample {
    UsageSample {
        workload_id: workload,
        provider_sample_id: format!("soak-{sequence}"),
        provider: "a1002".to_owned(),
        collected_at_millis: sequence * 1_000,
        cumulative: BTreeMap::from([(
            Dimension::CpuMillis,
            u64::try_from(sequence).unwrap() * 100,
        )]),
    }
}

#[test]
fn settlement_soak_has_no_duplicates_or_unbalanced_records() {
    let integration = aseman_config::IntegrationTestConfig::from_process();
    if !integration.run_soak_tests {
        eprintln!("ASEMAN_RUN_SOAK_TESTS is absent; skipping settlement soak");
        return;
    }
    let admin_uri = integration
        .postgres_url
        .expect("ASEMAN_TEST_POSTGRES_URL is required for settlement soak");
    let database = format!("aseman_finance_soak_{}", uuid::Uuid::now_v7().simple());
    let mut admin = Client::connect(&admin_uri, NoTls).unwrap();
    admin
        .batch_execute(&format!("CREATE DATABASE {database}"))
        .unwrap();
    let mut config = Config::from_str(&admin_uri).unwrap();
    config.dbname(&database);
    let finance = PostgresFinance::connect_config(config, 16).unwrap();
    finance.migrate().unwrap();
    finance
        .publish(&PriceList {
            version: "soak-v1".to_owned(),
            effective_from_millis: 0,
            rates: BTreeMap::from([(Dimension::CpuMillis, 1_000_000)]),
        })
        .unwrap();

    let workload = Uuid::now_v7();
    let baseline = sample(workload, 0);
    finance.record_sample(&baseline).unwrap();
    let mut previous = baseline;
    for sequence in 1..=INTERVALS {
        let current = sample(workload, sequence);
        finance.record_sample(&current).unwrap();
        let interval = interval_between(&previous, &current).unwrap();
        finance.record_interval(&interval).unwrap();
        let list = finance.price_lists().unwrap().remove(0);
        let charge = price(&interval, &list).unwrap();
        let record = settle(
            &charge,
            "wallet:soak",
            "revenue:soak",
            current.collected_at_millis,
        )
        .unwrap();
        assert!(record.balances());
        finance.commit(&record).unwrap();
        finance.commit(&record).unwrap();
        previous = current;
    }
    assert!(finance.unsettled(1).unwrap().is_empty());
    let records = finance
        .settlements(workload, INTERVALS as usize + 1)
        .unwrap();
    assert_eq!(records.len(), INTERVALS as usize);
    assert!(records.iter().all(|record| record.balances()));

    drop(finance);
    admin
        .batch_execute(&format!("DROP DATABASE {database} WITH (FORCE)"))
        .unwrap();
}
