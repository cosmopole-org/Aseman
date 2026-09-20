//! Stable module manifest, negotiation, lifecycle, and recovery wire values.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleKind {
    Storage,
    ClientNetwork,
    Federation,
    Security,
    Realtime,
    FinanceLedger,
    Consensus,
    Coordination,
    Vmm,
    VmmBackend,
    Runtime,
    Telemetry,
    Sample,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModulePermissions {
    #[serde(default)]
    pub network_egress: Vec<String>,
    #[serde(default)]
    pub network_listen: Vec<String>,
    #[serde(default)]
    pub read_only_mounts: Vec<String>,
    #[serde(default)]
    pub read_write_mounts: Vec<String>,
    #[serde(default)]
    pub secret_refs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModulePlatform {
    pub os: String,
    pub architecture: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleManifest {
    pub schema_version: u16,
    pub name: String,
    pub kind: ModuleKind,
    pub version: String,
    pub contract: String,
    pub artifact_digest: String,
    pub command: Vec<String>,
    pub health_endpoint: String,
    pub config_schema: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub permissions: ModulePermissions,
    #[serde(default)]
    pub platforms: Vec<ModulePlatform>,
    #[serde(default)]
    pub migrations: Vec<String>,
    pub sbom: String,
    pub license_manifest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureEnvelope {
    pub algorithm: String,
    pub key_id: String,
    pub signature_hex: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmodFile {
    pub path: String,
    pub content_base64: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AmodBundle {
    pub format_version: u16,
    pub manifest_toml: String,
    pub files: Vec<AmodFile>,
    pub signature: SignatureEnvelope,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolVersion {
    pub major: u16,
    pub minor: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleHandshake {
    pub protocol: ProtocolVersion,
    pub provider_kind: ModuleKind,
    pub implementation_version: String,
    pub instance_id: String,
    pub capabilities: Vec<String>,
    pub schema_digests: Vec<String>,
    pub max_message_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestMetadata {
    pub request_id: String,
    pub trace_id: String,
    pub deadline_unix_millis: i64,
    pub cancellation_id: String,
    pub idempotency_key: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModuleLifecycleState {
    Installed,
    Validated,
    Staged,
    Ready,
    Active,
    Draining,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapProvider {
    pub name: String,
    pub kind: ModuleKind,
    pub version: String,
    pub artifact_digest: String,
    pub endpoint: String,
    pub config_digest: String,
    pub secret_refs: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapTrustRoot {
    pub key_id: String,
    pub algorithm: String,
    pub public_key_hex: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootstrapSnapshot {
    pub schema_version: u16,
    pub node_id: String,
    pub routing_generation: u64,
    pub generated_at_unix_millis: i64,
    pub expires_at_unix_millis: i64,
    pub providers: Vec<BootstrapProvider>,
    pub trust_roots: Vec<BootstrapTrustRoot>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_manifest_fields_fail_closed() {
        let value = r#"{
            "schema_version":1,"name":"sample","kind":"sample","version":"1.0.0",
            "contract":">=1.0, <2.0","artifact_digest":"sha256:00",
            "command":["/bin/sample"],"health_endpoint":"/health/ready",
            "config_schema":"config.schema.json","sbom":"sbom.spdx.json",
            "license_manifest":"licenses.json","unexpected":true
        }"#;
        assert!(serde_json::from_str::<ModuleManifest>(value).is_err());
    }

    #[test]
    fn lifecycle_values_use_contract_spelling() {
        assert_eq!(
            serde_json::to_string(&ModuleLifecycleState::Draining).unwrap(),
            "\"draining\""
        );
    }
}
