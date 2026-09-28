//! ADR 0020: legacy operational raw keys are verified and never migrated.

use super::*;

const COUNTERS: [&str; 2] = ["globalIdCounter", "localIdCounter"];
const CALLBACK_PREFIX: &str = "chainCallback::";

/// `true` for a raw key family that ADR 0020 reviewed.
#[must_use]
pub fn is_reviewed_operational_raw_key(key: &str) -> bool {
    COUNTERS.contains(&key) || key.starts_with(CALLBACK_PREFIX)
}

impl LegacySnapshotGraph {
    /// Verify ID counters and dead chain-callback records; nothing here is exported.
    pub(crate) fn verify_legacy_operational_keys(
        &self,
        local_origins: &BTreeSet<String>,
    ) -> LegacyMigrationResult<()> {
        for (key, value) in &self.raw {
            if COUNTERS.contains(&key.as_str()) {
                let counter = <[u8; 8]>::try_from(value.as_slice())
                    .map(i64::from_be_bytes)
                    .ok()
                    .filter(|counter| *counter >= 0)
                    .ok_or_else(|| {
                        LegacyMigrationError::Invalid(format!(
                            "legacy {key} is not a non-negative 8-byte big-endian counter"
                        ))
                    })?;
                let highest = self.highest_minted_id(key == "globalIdCounter", local_origins);
                if counter < highest {
                    return Err(LegacyMigrationError::Invalid(format!(
                        "legacy {key} ({counter}) is below minted ID {highest}; legacy would mint duplicates"
                    )));
                }
            } else {
                verify_chain_callback(key, value)?;
            }
        }
        Ok(())
    }

    /// Largest `{n}` over object IDs `{n}@global` or `{n}@{declared local origin}`.
    fn highest_minted_id(&self, global: bool, local_origins: &BTreeSet<String>) -> i64 {
        self.objects
            .keys()
            .filter_map(|(_, id)| {
                let (counter, origin) = id.split_once('@')?;
                let counted = if global {
                    origin == "global"
                } else {
                    origin != "global" && local_origins.contains(origin)
                };
                if !counted
                    || counter.is_empty()
                    || !counter.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return None;
                }
                counter.parse::<i64>().ok()
            })
            .max()
            .unwrap_or(0)
    }
}

fn verify_chain_callback(key: &str, value: &[u8]) -> LegacyMigrationResult<()> {
    let rest = &key[CALLBACK_PREFIX.len()..];
    let valid = if rest.ends_with("::targetCount") || rest.ends_with("::tempCount") {
        value.len() == 4
    } else if rest.contains("|>") {
        value == [1]
    } else {
        rest.contains('|')
            && rest
                .rsplit_once("::")
                .is_some_and(|(_, field)| matches!(field, "machineId" | "storeId" | "attachment"))
            && std::str::from_utf8(value).is_ok()
    };
    if !valid {
        return Err(LegacyMigrationError::Invalid(format!(
            "legacy chain callback record {key} does not match the reviewed dead-state shape"
        )));
    }
    Ok(())
}
