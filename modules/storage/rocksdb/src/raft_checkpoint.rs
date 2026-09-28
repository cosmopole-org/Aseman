//! ADR 0012: the OpenRaft state machine is not a target authority. Its checkpoint is
//! read verbatim, digested canonically, and compared across replicas for the rollback
//! window; its free-form operator knobs are surfaced for typed-configuration review.

use super::*;
use rocksdb::{ColumnFamilyDescriptor, DB as ClusterDb};

const CLUSTER_COLUMN_FAMILIES: [&str; 3] = ["meta", "logs", "sm"];
const STATE_FIELDS: [&str; 3] = ["last_applied", "membership", "shared_config"];

/// A verified, read-only OpenRaft state-machine checkpoint from one replica.
#[derive(Clone, Debug, PartialEq)]
pub struct LegacyClusterCheckpoint {
    /// The state exactly as stored, as a canonical capsule value.
    pub state: CapsuleValue,
    /// Free-form `extra.*` operator knobs that must be mapped to typed configuration.
    pub operator_knobs: BTreeSet<String>,
    /// Domain-separated SHA-256 over the canonical encoding of `state`.
    pub digest: [u8; 32],
}

impl LegacyClusterCheckpoint {
    /// Parse the `sm/state` JSON strictly; an absent state is the legacy default.
    pub fn from_state_json(state: Option<&[u8]>) -> LegacyMigrationResult<Self> {
        let value = match state {
            Some(bytes) => serde_json::from_slice::<Value>(bytes).map_err(|error| {
                LegacyMigrationError::Invalid(format!("OpenRaft state is not JSON: {error}"))
            })?,
            None => serde_json::json!({
                "last_applied": null,
                "membership": null,
                "shared_config": {},
            }),
        };
        let Value::Object(fields) = &value else {
            return Err(LegacyMigrationError::Invalid(
                "OpenRaft state is not an object".to_owned(),
            ));
        };
        if fields
            .keys()
            .any(|field| !STATE_FIELDS.contains(&field.as_str()))
            || STATE_FIELDS
                .iter()
                .any(|field| !fields.contains_key(*field))
        {
            return Err(LegacyMigrationError::Invalid(
                "OpenRaft state fields differ from the reviewed checkpoint shape".to_owned(),
            ));
        }
        let Value::Object(knobs) = &fields["shared_config"] else {
            return Err(LegacyMigrationError::Invalid(
                "OpenRaft shared_config is not an object".to_owned(),
            ));
        };
        let operator_knobs = knobs.keys().cloned().collect();
        let state = legacy_json_to_capsule_value("OpenRaft state", &value)?;
        let encoded = encode_value(&state)
            .map_err(|error| LegacyMigrationError::Contract(error.to_string()))?;
        let mut hasher = Sha256::new();
        hasher.update(b"ASEMAN-LEGACY-OPENRAFT-CHECKPOINT-V1\0");
        hasher.update((encoded.len() as u64).to_be_bytes());
        hasher.update(&encoded);
        Ok(Self {
            state,
            operator_knobs,
            digest: hasher.finalize().into(),
        })
    }

    /// Read `<storage_root>/cluster/raft-db` without creating or modifying it.
    pub fn read_only(path: &Path) -> LegacyMigrationResult<Self> {
        let mut options = Options::default();
        options.create_if_missing(false);
        let families = CLUSTER_COLUMN_FAMILIES
            .iter()
            .map(|name| ColumnFamilyDescriptor::new(*name, Options::default()));
        let database = ClusterDb::open_cf_descriptors_read_only(&options, path, families, false)
            .map_err(|error| LegacyMigrationError::Storage(error.to_string()))?;
        let state_family = database.cf_handle("sm").ok_or_else(|| {
            LegacyMigrationError::Invalid("OpenRaft database has no sm column family".to_owned())
        })?;
        let state = database
            .get_cf(state_family, "state")
            .map_err(|error| LegacyMigrationError::Storage(error.to_string()))?;
        Self::from_state_json(state.as_deref())
    }
}

/// Every replica must hold the identical checkpoint before OpenRaft stops (ADR 0012).
pub fn verify_legacy_cluster_replicas(
    checkpoints: &[LegacyClusterCheckpoint],
) -> LegacyMigrationResult<[u8; 32]> {
    let first = checkpoints.first().ok_or_else(|| {
        LegacyMigrationError::Invalid("no OpenRaft replica checkpoint was supplied".to_owned())
    })?;
    if let Some(index) = checkpoints
        .iter()
        .position(|checkpoint| checkpoint.digest != first.digest)
    {
        return Err(LegacyMigrationError::Invalid(format!(
            "OpenRaft replica {index} diverges from replica 0; stop writes and resynchronize"
        )));
    }
    Ok(first.digest)
}
