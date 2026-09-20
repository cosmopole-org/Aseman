//! Plugin metadata — the parsed form of each VM project's `vm.config.json`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

fn default_entity_file_name() -> String {
    "module.wasm".to_string()
}

/// Descriptor of a VM plugin, loaded from the plugin project's
/// `vm.config.json` file.
///
/// `key` is the canonical runtime name the rest of the platform uses to
/// address this VM type (e.g. `fire` for Firecracker, `javascript` for
/// QuickJS). Every other field describes, declaratively, how the Caspar node
/// must treat programs of this runtime — so no per-VM knowledge ever needs to
/// be hardcoded in the node itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VmPluginMeta {
    /// Canonical runtime key (lowercase), e.g. `"docker"`, `"fire"`, `"wasm"`.
    pub key: String,
    /// Human readable name.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// Alternative runtime hints that resolve to this plugin
    /// (e.g. `"firecracker"` for `fire`, `"quickjs"` for `javascript`).
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Artifact path suffixes claimed by this runtime
    /// (e.g. `".masm"` for elpify, `".elpian.json"` for elpian).
    #[serde(default)]
    pub artifact_extensions: Vec<String>,
    /// True when VMs of this runtime execute inside the node process
    /// (the "managed" runtimes). False for externally supervised VMs
    /// such as docker containers.
    #[serde(default)]
    pub in_process: bool,
    /// True for the fallback runtime used when no hint or artifact
    /// extension matches (historically `wasm`).
    #[serde(default)]
    pub default_runtime: bool,
    /// Primary file name a deployed entity of this runtime is stored as
    /// (e.g. `Dockerfile`, `module.wasm`, `module.elpify.js`).
    #[serde(default = "default_entity_file_name")]
    pub entity_file_name: String,
    /// Whether deploy accepts an additional `files` map beside the primary
    /// payload.
    #[serde(default)]
    pub accepts_extra_files: bool,
    /// Whether a deploy must be followed by an asynchronous image/module
    /// build (`buildVmImage`).
    #[serde(default)]
    pub build_on_deploy: bool,
    /// Whether deploy records `vmEntityPath`/`vmEntityType` links so signal
    /// dispatch can resolve the entity's module path.
    #[serde(default)]
    pub set_entity_links_on_deploy: bool,
    /// Whether this runtime executes grouped chain transactions.
    #[serde(default)]
    pub supports_chain_trxs: bool,
    /// Whether previously running VMs of this runtime are restarted when the
    /// node restores a snapshot.
    #[serde(default)]
    pub restorable: bool,
    /// Legacy container ABI: when an exec/copy/build packet carries no
    /// resolvable runtime hint, the plugin with this flag handles it.
    #[serde(default)]
    pub exec_fallback: bool,
    /// Whether this runtime can verify program execution proofs
    /// (`verifyProgramExecution` packets are routed to it).
    #[serde(default)]
    pub provides_program_verification: bool,
}

impl VmPluginMeta {
    /// Parse a `vm.config.json` document.
    pub fn from_config_str(raw: &str) -> Result<Self, String> {
        let mut meta: VmPluginMeta =
            serde_json::from_str(raw).map_err(|e| format!("invalid vm.config.json: {}", e))?;
        meta.key = crate::util::normalize_runtime(&meta.key);
        if meta.key.is_empty() {
            return Err("vm.config.json: `key` is required".to_string());
        }
        meta.aliases = meta
            .aliases
            .iter()
            .map(|a| crate::util::normalize_runtime(a))
            .filter(|a| !a.is_empty())
            .collect();
        Ok(meta)
    }

    /// Whether `hint` (a runtime name from a packet or a program record)
    /// addresses this plugin, by key or alias.
    pub fn matches_key(&self, hint: &str) -> bool {
        let hint = crate::util::normalize_runtime(hint);
        !hint.is_empty() && (self.key == hint || self.aliases.iter().any(|a| a == &hint))
    }

    /// Whether an artifact path (module/AST file) belongs to this runtime by
    /// file extension.
    pub fn matches_artifact(&self, path: &str) -> bool {
        !path.is_empty()
            && self
                .artifact_extensions
                .iter()
                .any(|ext| !ext.is_empty() && path.ends_with(ext.as_str()))
    }

    /// JSON view of the deploy behaviour, consumed by the node's program
    /// deploy action.
    pub fn deploy_spec_json(&self) -> Value {
        json!({
            "runtime": self.key,
            "entityFileName": self.entity_file_name,
            "acceptsExtraFiles": self.accepts_extra_files,
            "buildOnDeploy": self.build_on_deploy,
            "setEntityLinksOnDeploy": self.set_entity_links_on_deploy,
        })
    }
}
