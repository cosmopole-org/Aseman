//! Framework-neutral observability vocabulary.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct OperationContext {
    pub request_id: String,
    pub trace_id: String,
    pub node_id: Option<String>,
    pub creature_id: Option<String>,
    pub workload_id: Option<String>,
    pub operation_id: Option<String>,
}

/// Which readiness decision consumes a dependency check (A904).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessScope {
    Api,
    Singleton,
    Both,
}

/// One dependency result. Optional degradation is visible but does not eject a node.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DependencyHealth {
    pub name: String,
    pub scope: ReadinessScope,
    pub required: bool,
    pub healthy: bool,
    pub reason: String,
}

/// Framework-neutral health state. Liveness deliberately has no dependency input.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HealthReport {
    pub live: bool,
    pub api_ready: bool,
    pub singleton_eligible: bool,
    pub checks: Vec<DependencyHealth>,
}

impl HealthReport {
    /// Evaluate API readiness separately from singleton-worker eligibility.
    #[must_use]
    pub fn evaluate(live: bool, checks: Vec<DependencyHealth>) -> Self {
        let required_healthy = |scope| {
            checks.iter().filter(|check| check.required).all(|check| {
                let applies = matches!(check.scope, ReadinessScope::Both) || check.scope == scope;
                !applies || check.healthy
            })
        };
        let api_ready = live && required_healthy(ReadinessScope::Api);
        let singleton_eligible = api_ready && required_healthy(ReadinessScope::Singleton);
        Self {
            live,
            api_ready,
            singleton_eligible,
            checks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, scope: ReadinessScope, required: bool, healthy: bool) -> DependencyHealth {
        DependencyHealth {
            name: name.to_owned(),
            scope,
            required,
            healthy,
            reason: if healthy { "ok" } else { "unavailable" }.to_owned(),
        }
    }

    #[test]
    fn singleton_failure_does_not_remove_a_serving_api_replica() {
        let report = HealthReport::evaluate(
            true,
            vec![
                check("postgres", ReadinessScope::Both, true, true),
                check("coordination", ReadinessScope::Singleton, true, false),
            ],
        );
        assert!(report.live);
        assert!(report.api_ready);
        assert!(!report.singleton_eligible);
    }

    #[test]
    fn required_api_failure_is_not_hidden_by_liveness() {
        let report = HealthReport::evaluate(
            true,
            vec![check("identity", ReadinessScope::Api, true, false)],
        );
        assert!(report.live);
        assert!(!report.api_ready);
        assert!(!report.singleton_eligible);
    }

    #[test]
    fn optional_dependency_is_degraded_but_ready() {
        let report = HealthReport::evaluate(
            true,
            vec![check(
                "telemetry-export",
                ReadinessScope::Both,
                false,
                false,
            )],
        );
        assert!(report.api_ready);
        assert!(report.singleton_eligible);
        assert!(!report.checks[0].healthy);
    }
}
