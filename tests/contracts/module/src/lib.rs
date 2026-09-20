//! Reusable conformance checks for independently implemented Aseman modules.
#![forbid(unsafe_code)]

use aseman_contracts::module::{ModuleHandshake, ModuleKind, ModuleManifest, ProtocolVersion};
use aseman_module_runtime::{
    ConformanceReport, ConformanceSuite, ModuleError, ModuleResult, negotiate, validate_manifest,
};
use std::collections::BTreeSet;
use std::path::Path;

pub const SUITE_VERSION: &str = "module-v1";

#[derive(Clone, Debug)]
pub struct ModuleConformanceKit {
    pub expected_kind: ModuleKind,
    pub required_capabilities: BTreeSet<String>,
    pub max_message_bytes: u64,
}

impl ModuleConformanceKit {
    pub fn validate_handshake(&self, provider: &ModuleHandshake) -> ModuleResult<()> {
        let client = ModuleHandshake {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            provider_kind: self.expected_kind,
            implementation_version: SUITE_VERSION.to_owned(),
            instance_id: "conformance-client".to_owned(),
            capabilities: self.required_capabilities.iter().cloned().collect(),
            schema_digests: Vec::new(),
            max_message_bytes: self.max_message_bytes,
        };
        let negotiated = negotiate(&client, provider, &self.required_capabilities)?;
        if negotiated.max_message_bytes > self.max_message_bytes {
            return Err(ModuleError::Conformance(
                "provider widened the negotiated message bound".to_owned(),
            ));
        }
        Ok(())
    }
}

impl ConformanceSuite for ModuleConformanceKit {
    fn validate(
        &self,
        manifest: &ModuleManifest,
        artifact: &Path,
    ) -> ModuleResult<ConformanceReport> {
        validate_manifest(manifest)?;
        if manifest.kind != self.expected_kind {
            return Err(ModuleError::Conformance(
                "manifest provider kind does not match suite".to_owned(),
            ));
        }
        if !artifact.is_file() {
            return Err(ModuleError::Conformance(
                "verified artifact is absent from cache".to_owned(),
            ));
        }
        let capabilities: BTreeSet<&str> =
            manifest.capabilities.iter().map(String::as_str).collect();
        if self
            .required_capabilities
            .iter()
            .any(|required| !capabilities.contains(required.as_str()))
        {
            return Err(ModuleError::Conformance(
                "required manifest capability is absent".to_owned(),
            ));
        }
        Ok(ConformanceReport {
            suite_version: SUITE_VERSION.to_owned(),
            passed_cases: vec![
                "manifest.closed".to_owned(),
                "artifact.verified-cache".to_owned(),
                "capabilities.required".to_owned(),
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aseman_module_runtime::parse_manifest;
    use std::fs;

    #[test]
    fn checked_in_sample_manifest_passes_shared_static_checks() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../");
        let manifest_source =
            fs::read_to_string(root.join("contracts/module/fixtures/valid/sample-module.toml"))
                .unwrap();
        let manifest = parse_manifest(&manifest_source).unwrap();
        let artifact = root.join("modules/sample-provider/src/main.rs");
        let kit = ModuleConformanceKit {
            expected_kind: ModuleKind::Sample,
            required_capabilities: BTreeSet::from([
                "sample.echo".to_owned(),
                "sample.health".to_owned(),
            ]),
            max_message_bytes: 16 * 1024,
        };
        let report = kit.validate(&manifest, &artifact).unwrap();
        assert_eq!(report.suite_version, SUITE_VERSION);
    }

    #[test]
    fn handshake_missing_a_required_capability_fails() {
        let kit = ModuleConformanceKit {
            expected_kind: ModuleKind::Sample,
            required_capabilities: BTreeSet::from(["sample.echo".to_owned()]),
            max_message_bytes: 4096,
        };
        let provider = ModuleHandshake {
            protocol: ProtocolVersion { major: 1, minor: 0 },
            provider_kind: ModuleKind::Sample,
            implementation_version: "1.0.0".to_owned(),
            instance_id: "sample".to_owned(),
            capabilities: vec!["sample.health".to_owned()],
            schema_digests: Vec::new(),
            max_message_bytes: 4096,
        };
        assert!(kit.validate_handshake(&provider).is_err());
    }
}
