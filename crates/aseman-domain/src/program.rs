//! Program values shared by the program use cases and their adapters.
//!
//! Program and machine identities are legacy string identities; capsule adapters map
//! them to canonical IDs. A machine may own several programs.

use serde::{Deserialize, Serialize};

/// One program as the wire exposes it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProgramRecord {
    pub id: String,
    /// The machine creature that owns the program.
    pub machine_id: String,
    pub runtime: String,
    pub path: String,
    pub comment: String,
}

/// The entity an alarm without one replays.
pub const DEFAULT_ALARM_ENTITY: &str = "main";

/// A program's pending wake-up (`vmAlarm*`). A program has at most one.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProgramAlarm {
    /// The store the program runs in when it wakes.
    pub store_id: String,
    /// Unix milliseconds, as stored.
    pub fire_at_millis: i64,
    pub data: String,
    /// The entity to run; alarms without one run [`DEFAULT_ALARM_ENTITY`].
    pub entity: String,
}

/// A VM resource store (`Json::VmResourceStore`, target
/// `core.vm_resource_store`): a named document owned by a machine.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmResourceStore {
    pub id: String,
    pub name: String,
    /// The owning machine creature, or a program whose machine owns it.
    pub machine_id: String,
    /// The metadata document as compact JSON object text.
    pub metadata: String,
}

/// A program entity (`Entity` object keyed `{program}::{entity}`, target
/// `core.entity`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct EntityRecord {
    pub program_id: String,
    pub entity_id: String,
    /// The runtime that executes the entity.
    pub entity_type: String,
    pub image_name: String,
}

/// The role of a deployed entity file (`core.entity_artifact.artifact_role`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ArtifactRole {
    /// The file the runtime executes (`vmEntityPath`).
    Primary,
    /// The file clients fetch (`vmEntityDownloadable`).
    Downloadable,
}

impl ArtifactRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Downloadable => "downloadable",
        }
    }
}

/// A deployed entity file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EntityArtifact {
    /// The blob key of the file's bytes (ADR 0027). `None` when the migration
    /// attested that the bytes were already missing.
    pub store_key: Option<String>,
}

/// The identity of a VM resource entity (`Json::VmResourceEntity::{store}::
/// {type}::{id}`, target `core.vm_resource_entity`).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResourceEntityRef {
    pub store_id: String,
    pub entity_type: String,
    pub entity_id: String,
}

impl ResourceEntityRef {
    /// Every part is one blob-key segment and cannot split the legacy key (LD-26):
    /// guest-supplied parts name the entity's data file.
    pub fn is_valid(&self) -> bool {
        [&self.store_id, &self.entity_type, &self.entity_id]
            .iter()
            .all(|part| {
                !part.contains("::") && !part.contains('/') && crate::blob::valid_blob_key(part)
            })
    }

    /// The legacy identity `{store}::{type}::{id}`.
    pub fn legacy_id(&self) -> String {
        [
            &self.store_id,
            "::",
            &self.entity_type,
            "::",
            &self.entity_id,
        ]
        .concat()
    }

    /// The blob key of the entity's data, at the file's path.
    pub fn data_key(&self) -> String {
        [
            "vm_stores/",
            &self.store_id,
            "/",
            &self.entity_type,
            "/",
            &self.entity_id,
            ".json",
        ]
        .concat()
    }
}

/// A VM resource entity: a payload document and a data file in a resource store.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmResourceEntity {
    pub reference: ResourceEntityRef,
    /// The payload document as compact JSON object text.
    pub payload: String,
    /// The blob key of the data. `None` when the migration attested that the bytes
    /// were already missing.
    pub data_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_entity_parts_are_single_key_segments() {
        let reference = |store: &str, kind: &str, id: &str| ResourceEntityRef {
            store_id: store.to_owned(),
            entity_type: kind.to_owned(),
            entity_id: id.to_owned(),
        };
        let valid = reference("vs-1", "doc", "e-1");
        assert!(valid.is_valid());
        assert_eq!(valid.legacy_id(), "vs-1::doc::e-1");
        assert_eq!(valid.data_key(), "vm_stores/vs-1/doc/e-1.json");
        for invalid in [
            reference("", "doc", "e"),
            reference("vs", "../..", "e"),
            reference("vs", "doc", "a/b"),
            reference("vs", "a::b", "e"),
            reference("..", "doc", "e"),
        ] {
            assert!(!invalid.is_valid(), "{invalid:?}");
        }
    }
}
