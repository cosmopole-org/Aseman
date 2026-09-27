//! Framework-neutral observability vocabulary.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

const HISTOGRAM_BUCKETS: [f64; 10] = [
    0.01,
    0.05,
    0.1,
    0.25,
    0.5,
    1.0,
    2.5,
    5.0,
    10.0,
    f64::INFINITY,
];

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct MetricKey {
    name: &'static str,
    labels: Vec<(&'static str, String)>,
}

#[derive(Clone, Debug)]
struct Histogram {
    buckets: [u64; HISTOGRAM_BUCKETS.len()],
    count: u64,
    sum: f64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: [0; HISTOGRAM_BUCKETS.len()],
            count: 0,
            sum: 0.0,
        }
    }
}

/// Process-local metric storage rendered on the internal telemetry endpoint. Labels
/// are supplied only by call sites from A905's bounded vocabulary.
#[derive(Default)]
pub struct MetricRegistry {
    counters: Mutex<BTreeMap<MetricKey, u64>>,
    gauges: Mutex<BTreeMap<MetricKey, f64>>,
    histograms: Mutex<BTreeMap<MetricKey, Histogram>>,
}

fn key(name: &'static str, labels: &[(&'static str, &str)]) -> MetricKey {
    MetricKey {
        name,
        labels: labels
            .iter()
            .map(|(name, value)| (*name, (*value).to_owned()))
            .collect(),
    }
}

fn labels_text(labels: &[(&'static str, String)], extra: Option<(&str, String)>) -> String {
    let mut values: Vec<String> = labels
        .iter()
        .map(|(name, value)| format!("{name}=\"{}\"", escape_label(value)))
        .collect();
    if let Some((name, value)) = extra {
        values.push(format!("{name}=\"{}\"", escape_label(&value)));
    }
    if values.is_empty() {
        String::new()
    } else {
        format!("{{{}}}", values.join(","))
    }
}

fn escape_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

impl MetricRegistry {
    pub fn increment(&self, name: &'static str, labels: &[(&'static str, &str)]) {
        let mut counters = self
            .counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *counters.entry(key(name, labels)).or_default() += 1;
    }

    pub fn set_gauge(&self, name: &'static str, labels: &[(&'static str, &str)], value: f64) {
        self.gauges
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key(name, labels), value);
    }

    pub fn observe(&self, name: &'static str, labels: &[(&'static str, &str)], value: f64) {
        let mut histograms = self
            .histograms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let histogram = histograms.entry(key(name, labels)).or_default();
        histogram.count += 1;
        histogram.sum += value.max(0.0);
        for (index, bound) in HISTOGRAM_BUCKETS.iter().enumerate() {
            if value <= *bound {
                histogram.buckets[index] += 1;
            }
        }
    }

    #[must_use]
    pub fn render_prometheus(&self) -> String {
        let mut output = String::new();
        for (key, value) in self
            .counters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            output.push_str(&format!(
                "{}{} {}\n",
                key.name,
                labels_text(&key.labels, None),
                value
            ));
        }
        for (key, value) in self
            .gauges
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            output.push_str(&format!(
                "{}{} {}\n",
                key.name,
                labels_text(&key.labels, None),
                value
            ));
        }
        for (key, histogram) in self
            .histograms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            for (index, bound) in HISTOGRAM_BUCKETS.iter().enumerate() {
                let bound = if bound.is_infinite() {
                    "+Inf".to_owned()
                } else {
                    bound.to_string()
                };
                output.push_str(&format!(
                    "{}_bucket{} {}\n",
                    key.name,
                    labels_text(&key.labels, Some(("le", bound))),
                    histogram.buckets[index]
                ));
            }
            output.push_str(&format!(
                "{}_sum{} {}\n{}_count{} {}\n",
                key.name,
                labels_text(&key.labels, None),
                histogram.sum,
                key.name,
                labels_text(&key.labels, None),
                histogram.count
            ));
        }
        output
    }
}

#[must_use]
pub fn metrics() -> &'static MetricRegistry {
    static METRICS: OnceLock<MetricRegistry> = OnceLock::new();
    METRICS.get_or_init(MetricRegistry::default)
}

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

    #[test]
    fn prometheus_registry_emits_bounded_labels_and_histogram_buckets() {
        let registry = MetricRegistry::default();
        registry.increment(
            "aseman_http_requests_total",
            &[("service", "node"), ("status_class", "2xx")],
        );
        registry.observe(
            "aseman_http_request_duration_seconds",
            &[("service", "node")],
            0.2,
        );
        registry.set_gauge("aseman_ready", &[("service", "node")], 1.0);
        let rendered = registry.render_prometheus();
        assert!(
            rendered
                .contains("aseman_http_requests_total{service=\"node\",status_class=\"2xx\"} 1")
        );
        assert!(rendered.contains(
            "aseman_http_request_duration_seconds_bucket{service=\"node\",le=\"0.25\"} 1"
        ));
        assert!(
            rendered.contains("aseman_http_request_duration_seconds_count{service=\"node\"} 1")
        );
        assert!(rendered.contains("aseman_ready{service=\"node\"} 1"));
    }
}
