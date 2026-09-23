//! The A807 golden cases, run against the domain itself.
//!
//! `tests/contracts/finance/golden-usage-to-journal.json` is the fixture every
//! implementation must reproduce: the same samples through the same price list produce
//! the same charge and the same journal record. A case that changes there is a billing
//! change, and this test is what makes changing one deliberate.
//!
//! The fixture is parsed by hand rather than with `serde_json`, because this crate may
//! depend only on serde, thiserror, and uuid (AGENTS.md). Hand-parsing a fixture is a
//! smaller price than widening the domain's dependencies.

use std::collections::BTreeMap;

use super::*;

const GOLDEN: &str =
    include_str!("../../../../tests/contracts/finance/golden-usage-to-journal.json");

/// The value of `"key": <number>` inside `slice`, when it is there.
fn number(slice: &str, key: &str) -> Option<i64> {
    let at = slice.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = slice[at..].trim_start().strip_prefix(':')?.trim_start();
    let end = rest
        .find(|character: char| !character.is_ascii_digit() && character != '-')
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// The object `"key": { ... }` inside `slice`, as text.
fn object<'a>(slice: &'a str, key: &str) -> Option<&'a str> {
    let at = slice.find(&format!("\"{key}\""))?;
    let open = slice[at..].find('{')? + at;
    let mut depth = 0;
    for (index, character) in slice[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&slice[open..=open + index]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Every `"dimension": <number>` pair in an object.
fn counters(slice: &str) -> BTreeMap<Dimension, u64> {
    let mut found = BTreeMap::new();
    for (name, dimension) in [
        ("cpu_millis", Dimension::CpuMillis),
        ("memory_mib_seconds", Dimension::MemoryMibSeconds),
        ("storage_mib_seconds", Dimension::StorageMibSeconds),
        ("disk_read_bytes", Dimension::DiskReadBytes),
        ("disk_write_bytes", Dimension::DiskWriteBytes),
        ("network_ingress_bytes", Dimension::NetworkIngressBytes),
        ("network_egress_bytes", Dimension::NetworkEgressBytes),
        ("accelerator_millis", Dimension::AcceleratorMillis),
    ] {
        if let Some(value) = number(slice, name) {
            found.insert(dimension, u64::try_from(value).unwrap_or(0));
        }
    }
    found
}

/// Split the `cases` array into its objects.
fn cases(document: &str) -> Vec<&str> {
    let Some(at) = document.find("\"cases\"") else {
        return Vec::new();
    };
    let array = &document[at..];
    let mut found = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (index, character) in array.char_indices() {
        match character {
            '{' => {
                if depth == 0 {
                    start = index;
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    found.push(&array[start..=index]);
                }
            }
            _ => {}
        }
    }
    found
}

fn sample(slice: &str, workload: Uuid) -> UsageSample {
    UsageSample {
        workload_id: workload,
        provider_sample_id: "fixture".to_owned(),
        provider: "fixture".to_owned(),
        collected_at_millis: number(slice, "collected_at_millis").expect("a timestamp"),
        cumulative: counters(object(slice, "cumulative").expect("counters")),
    }
}

#[test]
fn every_golden_case_still_prices_and_settles_the_same_way() {
    let list = object(GOLDEN, "price_list").expect("the price list");
    let prices = PriceList {
        version: "2026-09".to_owned(),
        effective_from_millis: 0,
        rates: counters(object(list, "rates").expect("rates")),
    };
    prices
        .validate()
        .expect("the fixture's prices are chargeable");
    assert_eq!(
        number(GOLDEN, "price_scale").expect("the scale"),
        i64::try_from(PRICE_SCALE).expect("the scale fits"),
        "the fixture and the code must agree about the scale"
    );

    let workload = Uuid::from_bytes([1; 16]);
    let found = cases(GOLDEN);
    assert_eq!(found.len(), 6, "every case runs");

    for case in found {
        let name = case
            .split("\"name\"")
            .nth(1)
            .and_then(|rest| rest.split('"').nth(1))
            .unwrap_or("a case");
        let previous = sample(object(case, "previous").expect("previous"), workload);
        let current = sample(object(case, "current").expect("current"), workload);
        let expect = object(case, "expect").expect("expectations");

        let interval =
            interval_between(&previous, &current).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            interval.interval_start_millis,
            number(expect, "interval_start_millis").expect("a start"),
            "{name}: interval start"
        );
        assert_eq!(
            interval.deltas,
            counters(object(expect, "deltas").expect("deltas")),
            "{name}: deltas"
        );

        let charge = price(&interval, &prices).unwrap_or_else(|error| panic!("{name}: {error}"));
        let expected_lines: BTreeMap<Dimension, Minor> =
            counters(object(expect, "lines").expect("lines"))
                .into_iter()
                .map(|(dimension, value)| (dimension, Minor(i64::try_from(value).expect("a line"))))
                .collect();
        assert_eq!(charge.lines, expected_lines, "{name}: lines");
        assert_eq!(
            charge.amount,
            Minor(number(expect, "amount").expect("an amount")),
            "{name}: amount"
        );

        let record = settle(&charge, "wallet:fixture", "revenue:fixture", 0)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(record.balances(), "{name}: the journal record balances");
        assert_eq!(
            record.entries[0].amount,
            Minor(-charge.amount.0),
            "{name}: the wallet side"
        );
        assert_eq!(
            record.entries[1].amount, charge.amount,
            "{name}: the revenue side"
        );
    }
}
