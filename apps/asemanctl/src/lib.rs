//! Canonical administration helpers that do not belong in the compatibility CLI.

pub mod cli;

/// Run the canonical administrative CLI.
pub fn main() {
    cli::main();
}

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::sync::OnceLock;
use thiserror::Error;

const REDACTION_CONTRACT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/operations/support-bundle-redaction.json"
));

#[derive(Debug, Deserialize)]
struct RedactionPolicy {
    version: u32,
    replacement: String,
    case_insensitive_key_fragments: Vec<String>,
    value_patterns: Vec<String>,
}

#[derive(Debug)]
struct CompiledPolicy {
    replacement: String,
    key_fragments: Vec<String>,
    value_patterns: Vec<Regex>,
}

#[derive(Debug, Error)]
pub enum RedactionError {
    #[error("invalid embedded support-bundle redaction contract: {0}")]
    InvalidContract(String),
}

fn policy() -> Result<&'static CompiledPolicy, RedactionError> {
    static POLICY: OnceLock<Result<CompiledPolicy, String>> = OnceLock::new();
    POLICY
        .get_or_init(|| {
            let contract: RedactionPolicy =
                serde_json::from_str(REDACTION_CONTRACT).map_err(|error| error.to_string())?;
            if contract.version != 1 {
                return Err(format!("unsupported version {}", contract.version));
            }
            let value_patterns = contract
                .value_patterns
                .iter()
                .map(|pattern| Regex::new(pattern).map_err(|error| error.to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CompiledPolicy {
                replacement: contract.replacement,
                key_fragments: contract.case_insensitive_key_fragments,
                value_patterns,
            })
        })
        .as_ref()
        .map_err(|message| RedactionError::InvalidContract(message.clone()))
}

/// Redact a structured support-bundle document in place.
///
/// Callers must still obey the contract's `never_collect` list; redaction is a second
/// boundary, not permission to read secrets.
pub fn redact_support_bundle(value: &mut Value) -> Result<(), RedactionError> {
    fn walk(value: &mut Value, policy: &CompiledPolicy) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields {
                    let normalized = key.to_ascii_lowercase();
                    if policy
                        .key_fragments
                        .iter()
                        .any(|fragment| normalized.contains(fragment))
                    {
                        *value = Value::String(policy.replacement.clone());
                    } else {
                        walk(value, policy);
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    walk(item, policy);
                }
            }
            Value::String(text) => {
                for pattern in &policy.value_patterns {
                    *text = pattern
                        .replace_all(text, policy.replacement.as_str())
                        .into_owned();
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    walk(value, policy()?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redacts_sensitive_keys_at_every_depth() {
        let mut document = json!({
            "status": "healthy",
            "nested": [{"Authorization": "Bearer visible"}],
            "database_password": "visible",
            "counts": {"workloads": 3}
        });
        redact_support_bundle(&mut document).unwrap();
        assert_eq!(document["status"], "healthy");
        assert_eq!(document["nested"][0]["Authorization"], "[REDACTED]");
        assert_eq!(document["database_password"], "[REDACTED]");
        assert_eq!(document["counts"]["workloads"], 3);
    }

    #[test]
    fn redacts_credentials_in_unstructured_values() {
        let mut document = json!({
            "log": "connect postgres://billing:hunter2@db/billing; Authorization: Bearer abc.def"
        });
        redact_support_bundle(&mut document).unwrap();
        let log = document["log"].as_str().unwrap();
        assert!(!log.contains("hunter2"));
        assert!(!log.contains("abc.def"));
        assert!(log.contains("[REDACTED]"));
    }
}
