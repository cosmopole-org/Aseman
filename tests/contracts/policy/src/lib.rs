//! A404 conformance: every policy provider must reproduce `decisions-v1.json` over the
//! A402 registry (ADR 0008). A provider passes when each case yields the expected
//! allow/deny, reason code, and explaining condition, with both versions reported.
#![forbid(unsafe_code)]

use aseman_domain::Uuid;
use aseman_domain::authority::{Condition, PolicyRequest, ResourceRef};
use aseman_domain::capability::{Grant, ResourceSelector};
use aseman_domain::identity::Subject;
use aseman_ports::PolicyDecisionPort;
use serde_json::Value;
use std::collections::BTreeSet;

fn subject_named(subjects: &Value, name: &str) -> Result<Subject, String> {
    subjects[name]
        .as_str()
        .ok_or_else(|| format!("unknown fixture subject {name}"))?
        .parse()
        .map_err(|_| format!("bad fixture subject {name}"))
}

fn names(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|name| name.as_str().map(str::to_owned))
        .collect()
}

fn uuid(value: &Value) -> Result<Option<Uuid>, String> {
    match value.as_str() {
        None => Ok(None),
        Some(text) => text
            .parse()
            .map(Some)
            .map_err(|_| format!("bad uuid {text}")),
    }
}

fn grant(subjects: &Value, value: &Value) -> Result<Grant, String> {
    let resource = &value["resource"];
    let kind = resource["kind"].as_str().unwrap_or_default().to_owned();
    Ok(Grant {
        id: uuid(&value["id"])?.ok_or("grant without id")?,
        subject: subject_named(subjects, value["subject"].as_str().unwrap_or_default())?,
        issuer: subject_named(subjects, value["issuer"].as_str().unwrap_or_default())?,
        actions: names(&value["actions"]),
        resource: match resource["scope"].as_str() {
            Some("exact") => ResourceSelector::Exact {
                kind,
                id: resource["id"].as_str().unwrap_or_default().to_owned(),
            },
            _ => ResourceSelector::AnyOfKind { kind },
        },
        delegable_actions: names(&value["delegable_actions"]),
        max_depth: u32::try_from(value["max_depth"].as_u64().unwrap_or(0)).unwrap_or(0),
        parent: uuid(&value["parent"])?,
        not_before_millis: value["not_before_millis"].as_i64().unwrap_or(i64::MAX),
        expires_at_millis: value["expires_at_millis"].as_i64(),
        revoked_at_millis: value["revoked_at_millis"].as_i64(),
        policy_version: "fixture".to_owned(),
    })
}

/// The normative fixtures.
pub const DECISIONS_V1: &str = include_str!("../decisions-v1.json");

/// Run every fixture against `provider`.
///
/// # Errors
///
/// One line per failing case, naming it and what differed.
pub fn check_provider(provider: &dyn PolicyDecisionPort) -> Result<usize, String> {
    let fixtures: Value = serde_json::from_str(DECISIONS_V1).map_err(|error| error.to_string())?;
    let subjects = &fixtures["subjects"];
    let mut failures = Vec::new();
    let cases = fixtures["cases"]
        .as_array()
        .ok_or("fixtures have no cases")?;
    for case in cases {
        let name = case["name"].as_str().unwrap_or("?");
        let subject = match case["subject"].as_str() {
            None => None,
            Some(kind) => {
                Some(subject_named(subjects, kind).map_err(|error| format!("{name}: {error}"))?)
            }
        };
        let grants = case["grants"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|chain| {
                chain
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|value| grant(subjects, value))
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("{name}: {error}"))?;
        let facts = case["facts"]
            .as_array()
            .ok_or_else(|| format!("{name}: no facts"))?
            .iter()
            .map(|fact| {
                fact.as_str()
                    .and_then(Condition::parse)
                    .ok_or_else(|| format!("{name}: unknown fact {fact}"))
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let request = PolicyRequest {
            subject,
            action: case["action"].as_str().unwrap_or_default().to_owned(),
            resource: ResourceRef {
                kind: case["resource"].as_str().unwrap_or_default().to_owned(),
                id: case["resource_id"].as_str().unwrap_or("fixture").to_owned(),
            },
            facts,
            grants,
            at_millis: 1_800_000_000_000,
        };
        let decision = match provider.decide(&request) {
            Ok(decision) => decision,
            Err(error) => {
                failures.push(format!("{name}: provider error {error}"));
                continue;
            }
        };
        let got = (
            decision.allowed,
            decision.reason.code(),
            decision.matched.map(Condition::as_str),
        );
        let expected = (
            case["allowed"].as_bool().unwrap_or(false),
            case["reason"].as_str().unwrap_or_default(),
            case["matched"].as_str(),
        );
        if got != expected {
            failures.push(format!("{name}: expected {expected:?}, got {got:?}"));
        }
        let expected_chain = case["grant_chain"]
            .as_array()
            .into_iter()
            .flatten()
            .map(uuid)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("{name}: {error}"))?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        if decision.grant_chain != expected_chain {
            failures.push(format!(
                "{name}: expected grant chain {expected_chain:?}, got {:?}",
                decision.grant_chain
            ));
        }
        if decision.registry_version.is_empty() || decision.policy_version.is_empty() {
            failures.push(format!("{name}: decision does not report its versions"));
        }
    }
    if failures.is_empty() {
        Ok(cases.len())
    } else {
        Err(failures.join("\n"))
    }
}
