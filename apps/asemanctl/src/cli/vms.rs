//! `asemanctl vms` — manage the node's pluggable VM types.
//!
//! The native VMM backend's runtimes are standalone Rust projects living in the
//! repository's `modules/runtime/` folder (each implementing the `aseman-vm-sdk`
//! interface). This module gives the host admin a convenient way to pick
//! which VM types the backend supports:
//!
//! * `asemanctl vms list`            — discover and show every VM project
//! * `asemanctl vms enable <key>`    — include a VM type in the next build
//! * `asemanctl vms disable <key>`   — exclude a VM type from the next build
//! * `asemanctl vms sync`            — regenerate the backend's plugin
//!   registration code (the build-time `aseman-vm-plugins` crate)
//! * `asemanctl vms new <key>`       — scaffold a new VM plugin project
//!
//! The enable/disable selection is stored in `modules/runtime/vms.state.json`; `sync`
//! regenerates the registration code from it, which is never edited by hand.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use aseman_config::CliConfig;
use serde_json::Value;

const STATE_FILE_NAME: &str = "vms.state.json";
const GENERATED_CRATE_DIR: &str = "crates/vm-plugins";

#[derive(Debug, Clone)]
struct VmProject {
    /// Canonical runtime key from vm.config.json.
    key: String,
    /// Cargo package name.
    package: String,
    /// Rust lib target name (identifier used in generated `use` paths).
    lib_name: String,
    name: String,
    version: String,
    dir: PathBuf,
    enabled: bool,
}

pub fn run_vms(config: &CliConfig, args: &[String]) -> Result<()> {
    if args.is_empty() {
        print_vms_usage();
        return Ok(());
    }
    match args[0].as_str() {
        "list" => cmd_list(config, &args[1..]),
        "enable" => cmd_set_enabled(config, &args[1..], true),
        "disable" => cmd_set_enabled(config, &args[1..], false),
        "sync" => cmd_sync(config, &args[1..]),
        "new" => cmd_new(config, &args[1..]),
        "help" | "-h" | "--help" => {
            print_vms_usage();
            Ok(())
        }
        other => {
            eprintln!("unknown vms subcommand \"{}\"\n", other);
            print_vms_usage();
            std::process::exit(1);
        }
    }
}

fn print_vms_usage() {
    println!(
        "asemanctl vms - manage the node's pluggable VM types\n\n\
         Usage:\n  asemanctl vms <subcommand> [flags]\n\n\
         Subcommands:\n  \
         list              Show every VM plugin project found in modules/runtime\n  \
         enable <key>      Include a VM type in the next node build\n  \
         disable <key>     Exclude a VM type from the next node build\n  \
         sync              Regenerate the node's VM plugin registration code\n  \
         new <key>         Scaffold a new VM plugin project in modules/runtime\n\n\
         Common flags:\n  \
         --vms-dir <path>  Path to the vms folder (default: auto-detected;\n                    \
         also honours the ASEMAN_VMS_DIR environment variable)\n  \
         --node-dir <path> Path to the native VMM backend (legacy flag name; sync only)"
    );
}

// ── Flag helpers (same convention as the rest of asemanctl) ────────────────

fn flag_value(args: &[String], name: &str) -> Option<String> {
    let long = format!("--{}", name);
    let long_eq = format!("--{}=", name);
    let mut i = 0;
    while i < args.len() {
        if args[i] == long {
            return args.get(i + 1).cloned();
        }
        if let Some(v) = args[i].strip_prefix(&long_eq) {
            return Some(v.to_string());
        }
        i += 1;
    }
    None
}

fn positional(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a.starts_with("--") {
            if !a.contains('=') {
                i += 1; // skip the flag's value
            }
        } else {
            out.push(a.clone());
        }
        i += 1;
    }
    out
}

// ── Discovery ───────────────────────────────────────────────────────────────

/// Resolve the vms folder: explicit flag, environment, then a search relative
/// to the working directory and the asemanctl binary.
fn resolve_vms_dir(config: &CliConfig, args: &[String]) -> Result<PathBuf> {
    if let Some(dir) = flag_value(args, "vms-dir") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return fs::canonicalize(&p).map_err(|e| anyhow!("{}: {}", p.display(), e));
        }
        bail!("--vms-dir does not exist: {}", p.display());
    }
    if let Some(dir) = &config.vms_dir {
        let p = PathBuf::from(dir.trim());
        if p.is_dir() {
            return fs::canonicalize(&p).map_err(|e| anyhow!("{}: {}", p.display(), e));
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        let mut base = Some(cwd);
        while let Some(dir) = base {
            candidates.push(dir.join("modules/runtime"));
            base = dir.parent().map(|p| p.to_path_buf());
        }
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("modules/runtime"));
        candidates.push(dir.join("../modules/runtime"));
    }
    for c in candidates {
        if c.is_dir() {
            return fs::canonicalize(&c).map_err(|e| anyhow!("{}: {}", c.display(), e));
        }
    }
    bail!("could not locate modules/runtime; pass --vms-dir or set ASEMAN_VMS_DIR")
}

fn read_disabled(vms_dir: &Path) -> BTreeSet<String> {
    let state_path = vms_dir.join(STATE_FILE_NAME);
    let Ok(raw) = fs::read_to_string(&state_path) else {
        return BTreeSet::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        return BTreeSet::new();
    };
    v["disabled"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(|s| s.trim().to_lowercase()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn write_disabled(vms_dir: &Path, disabled: &BTreeSet<String>) -> Result<()> {
    let state_path = vms_dir.join(STATE_FILE_NAME);
    let doc = serde_json::json!({
        "//": "Managed by `asemanctl vms enable|disable`. VM keys listed here are excluded from the node build.",
        "disabled": disabled.iter().collect::<Vec<_>>(),
    });
    fs::write(&state_path, serde_json::to_string_pretty(&doc)? + "\n")
        .map_err(|e| anyhow!("failed to write {}: {}", state_path.display(), e))
}

/// Extract `name = "..."` from a manifest section (`[package]` or `[lib]`).
fn manifest_name(manifest: &str, section: &str) -> Option<String> {
    let mut in_section = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed == format!("[{}]", section);
            continue;
        }
        if in_section && let Some(rest) = trimmed.strip_prefix("name") {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('=') {
                let v = rest.trim().trim_matches('"');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Discover every VM plugin project in the vms folder.
fn discover(vms_dir: &Path) -> Result<Vec<VmProject>> {
    let disabled = read_disabled(vms_dir);
    let mut projects = Vec::new();
    let entries = fs::read_dir(vms_dir)
        .map_err(|e| anyhow!("failed to read {}: {}", vms_dir.display(), e))?;
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let manifest_path = dir.join("Cargo.toml");
        let config_path = dir.join("vm.config.json");
        if !manifest_path.exists() || !config_path.exists() {
            continue;
        }
        let manifest = fs::read_to_string(&manifest_path)
            .map_err(|e| anyhow!("{}: {}", manifest_path.display(), e))?;
        let config_raw = fs::read_to_string(&config_path)
            .map_err(|e| anyhow!("{}: {}", config_path.display(), e))?;
        let config: Value = serde_json::from_str(&config_raw)
            .map_err(|e| anyhow!("invalid {}: {}", config_path.display(), e))?;
        let key = config["key"].as_str().unwrap_or("").trim().to_lowercase();
        if key.is_empty() {
            eprintln!(
                "warning: skipping {} — vm.config.json has no `key`",
                dir.display()
            );
            continue;
        }
        let package = match manifest_name(&manifest, "package") {
            Some(p) => p,
            None => {
                eprintln!(
                    "warning: skipping {} — Cargo.toml has no [package] name",
                    dir.display()
                );
                continue;
            }
        };
        let lib_name = manifest_name(&manifest, "lib").unwrap_or_else(|| package.replace('-', "_"));
        projects.push(VmProject {
            enabled: !disabled.contains(&key),
            key,
            name: config["name"].as_str().unwrap_or("").to_string(),
            version: config["version"].as_str().unwrap_or("").to_string(),
            package,
            lib_name,
            dir,
        });
    }
    projects.sort_by(|a, b| a.key.cmp(&b.key));
    if projects.is_empty() {
        bail!(
            "no VM plugin projects found in {} (each needs Cargo.toml + vm.config.json)",
            vms_dir.display()
        );
    }
    Ok(projects)
}

// ── Subcommands ─────────────────────────────────────────────────────────────

fn cmd_list(config: &CliConfig, args: &[String]) -> Result<()> {
    let vms_dir = resolve_vms_dir(config, args)?;
    let projects = discover(&vms_dir)?;
    println!("VM plugin projects in {}\n", vms_dir.display());
    println!(
        "  {:<12} {:<10} {:<22} {:<9} NAME",
        "KEY", "STATE", "PACKAGE", "VERSION"
    );
    for p in &projects {
        println!(
            "  {:<12} {:<10} {:<22} {:<9} {}",
            p.key,
            if p.enabled { "enabled" } else { "DISABLED" },
            p.package,
            p.version,
            p.name
        );
    }
    println!(
        "\nUse `asemanctl vms enable|disable <key>` to change the selection,\n\
         then `asemanctl vms sync` to apply it."
    );
    Ok(())
}

fn cmd_set_enabled(config: &CliConfig, args: &[String], enable: bool) -> Result<()> {
    let keys = positional(args);
    if keys.is_empty() {
        bail!(
            "usage: asemanctl vms {} <key> [<key>...]",
            if enable { "enable" } else { "disable" }
        );
    }
    let vms_dir = resolve_vms_dir(config, args)?;
    let projects = discover(&vms_dir)?;
    let mut disabled = read_disabled(&vms_dir);
    for raw_key in keys {
        let key = raw_key.trim().to_lowercase();
        let Some(project) = projects.iter().find(|p| p.key == key) else {
            bail!(
                "unknown VM key '{}'; available: {}",
                key,
                projects
                    .iter()
                    .map(|p| p.key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        };
        if enable {
            disabled.remove(&key);
            println!("enabled VM type '{}' ({})", key, project.package);
        } else {
            let config_raw =
                fs::read_to_string(project.dir.join("vm.config.json")).unwrap_or_default();
            let config: Value = serde_json::from_str(&config_raw).unwrap_or(Value::Null);
            if config["defaultRuntime"].as_bool().unwrap_or(false) {
                eprintln!(
                    "warning: '{}' is the default runtime — disabling it leaves the node \
                     without a fallback VM for programs that don't name a runtime",
                    key
                );
            }
            disabled.insert(key.clone());
            println!("disabled VM type '{}' ({})", key, project.package);
        }
    }
    write_disabled(&vms_dir, &disabled)?;
    println!(
        "\nSelection saved to {}.\nRun `asemanctl vms sync` to apply it to the backend build.",
        vms_dir.join(STATE_FILE_NAME).display()
    );
    Ok(())
}

fn resolve_node_dir(args: &[String], vms_dir: &Path) -> Result<PathBuf> {
    if let Some(dir) = flag_value(args, "node-dir") {
        let p = PathBuf::from(dir);
        if p.is_dir() {
            return fs::canonicalize(&p).map_err(|e| anyhow!("{}: {}", p.display(), e));
        }
        bail!("--node-dir does not exist: {}", p.display());
    }
    let candidate = vms_dir
        .parent()
        .map(|p| p.join("vmm-backend/native"))
        .unwrap_or_default();
    if candidate.join("Cargo.toml").exists() {
        return fs::canonicalize(&candidate).map_err(|e| anyhow!("{}: {}", candidate.display(), e));
    }
    bail!("could not locate the native VMM backend; pass --node-dir")
}

/// Relative path from `from` to `to` (both absolute).
fn rel_path(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let mut common = 0;
    while common < from.len() && common < to.len() && from[common] == to[common] {
        common += 1;
    }
    let mut out = PathBuf::new();
    for _ in common..from.len() {
        out.push("..");
    }
    for c in &to[common..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Regenerate the `aseman-vm-plugins` aggregation crate from the enabled VM
/// plugin projects. This is the ONLY place plugin crates are imported into
/// the node build; the generated files are marked `@generated` and rewritten
/// on every build.
fn cmd_sync(config: &CliConfig, args: &[String]) -> Result<()> {
    let vms_dir = resolve_vms_dir(config, args)?;
    let node_dir = resolve_node_dir(args, &vms_dir)?;
    let projects = discover(&vms_dir)?;
    let enabled: Vec<&VmProject> = projects.iter().filter(|p| p.enabled).collect();
    if enabled.is_empty() {
        bail!(
            "every VM type is disabled — the node needs at least one VM plugin; \
             run `asemanctl vms enable <key>` first"
        );
    }
    if !enabled.iter().any(|p| {
        let raw = fs::read_to_string(p.dir.join("vm.config.json")).unwrap_or_default();
        serde_json::from_str::<Value>(&raw)
            .map(|c| c["defaultRuntime"].as_bool().unwrap_or(false))
            .unwrap_or(false)
    }) {
        eprintln!(
            "warning: no enabled VM type declares defaultRuntime — programs without an \
             explicit runtime will fail to resolve"
        );
    }

    let gen_dir = node_dir.join(GENERATED_CRATE_DIR);
    fs::create_dir_all(gen_dir.join("src"))
        .map_err(|e| anyhow!("failed to create {}: {}", gen_dir.display(), e))?;

    // The compatibility SDK is owned beside the runtime implementations.
    let sdk_dir = vms_dir.join("sdk");
    if !sdk_dir.join("Cargo.toml").exists() {
        bail!(
            "could not locate runtime compatibility SDK at {}",
            sdk_dir.display()
        );
    }
    let sdk_rel = rel_path(&gen_dir, &sdk_dir);
    // Every plugin is registered with the runtime configuration.
    let config_dir = vms_dir.join("../../crates/aseman-config");
    if !config_dir.join("Cargo.toml").exists() {
        bail!(
            "could not locate the configuration crate at {}",
            config_dir.display()
        );
    }
    let config_rel = rel_path(&gen_dir, &config_dir);

    // ── Cargo.toml ──
    let mut manifest = String::new();
    manifest.push_str(
        "# @generated by `asemanctl vms sync` — DO NOT EDIT BY HAND.\n\
         # Regenerated by `asemanctl vms sync` from the VM plugin projects discovered\n\
         # in `modules/runtime/` (minus the ones the host admin disabled via\n\
         # `asemanctl vms disable <key>`).\n\n\
         [package]\n\
         name = \"aseman-vm-plugins\"\n\
         version = \"0.1.0\"\n\
         edition = \"2021\"\n\
         license = \"MIT\"\n\
         description = \"GENERATED aggregation crate that compiles the enabled VM plugins into the native VMM backend and registers them.\"\n\n\
         [lib]\n\
         name = \"aseman_vm_plugins\"\n\
         path = \"src/lib.rs\"\n\n\
         [dependencies]\n",
    );
    manifest.push_str(&format!(
        "aseman-config = {{ path = \"{}\" }}\n",
        config_rel.display()
    ));
    manifest.push_str(&format!(
        "aseman-vm-sdk = {{ path = \"{}\" }}\n",
        sdk_rel.display()
    ));
    for p in &enabled {
        let dep_rel = rel_path(&gen_dir, &p.dir);
        manifest.push_str(&format!(
            "{} = {{ path = \"{}\" }}\n",
            p.package,
            dep_rel.display()
        ));
    }

    // ── src/lib.rs ──
    let mut lib = String::new();
    lib.push_str(
        "// @generated by `asemanctl vms sync` — DO NOT EDIT BY HAND.\n\
         //\n\
         // Regenerated by `asemanctl vms sync`. It imports each VM plugin project\n\
         // enabled in `modules/runtime/` and registers it with the aseman-vm-sdk\n\
         // plugin registry, so the native VMM backend carries exactly the VM types\n\
         // the host admin selected — without its source ever naming them.\n\n\
         use std::sync::Once;\n\n\
         static REGISTER: Once = Once::new();\n\n\
         /// Register every compiled VM plugin `enabled` accepts with the VMM plugin\n\
         /// registry, each configured from `config`. Idempotent — safe to call from\n\
         /// multiple init paths; the first call wins.\n\
         pub fn register_all(config: &aseman_config::RuntimeConfig, enabled: &dyn Fn(&str) -> bool) {\n    REGISTER.call_once(|| {\n",
    );
    for p in &enabled {
        lib.push_str(&format!(
            "        if enabled(\"{}\") {{\n            {}::register(config);\n        }}\n",
            p.key, p.lib_name
        ));
    }
    lib.push_str(
        "    });\n\
         }\n\n\
         /// The VM plugin keys compiled into this build.\n\
         pub fn enabled_vm_keys() -> Vec<&'static str> {\n    vec![\n",
    );
    for p in &enabled {
        lib.push_str(&format!("        \"{}\",\n", p.key));
    }
    lib.push_str("    ]\n}\n");

    fs::write(gen_dir.join("Cargo.toml"), manifest)
        .map_err(|e| anyhow!("failed to write generated Cargo.toml: {}", e))?;
    fs::write(gen_dir.join("src/lib.rs"), lib)
        .map_err(|e| anyhow!("failed to write generated lib.rs: {}", e))?;

    println!(
        "regenerated {} with {} VM plugin(s): {}",
        gen_dir.display(),
        enabled.len(),
        enabled
            .iter()
            .map(|p| p.key.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let excluded: Vec<&str> = projects
        .iter()
        .filter(|p| !p.enabled)
        .map(|p| p.key.as_str())
        .collect();
    if !excluded.is_empty() {
        println!("excluded (disabled): {}", excluded.join(", "));
    }
    Ok(())
}

fn cmd_new(config: &CliConfig, args: &[String]) -> Result<()> {
    let keys = positional(args);
    let Some(raw_key) = keys.first() else {
        bail!("usage: asemanctl vms new <key>");
    };
    let key = raw_key.trim().to_lowercase();
    if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("VM key must be a lowercase alphanumeric identifier");
    }
    let vms_dir = resolve_vms_dir(config, args)?;
    let dir = vms_dir.join(&key);
    if dir.exists() {
        bail!("{} already exists", dir.display());
    }
    let package = format!("aseman-vm-{}", key.replace('_', "-"));
    let lib_name = package.replace('-', "_");
    let struct_name: String = {
        let mut s = String::new();
        let mut upper = true;
        for c in key.chars() {
            if c == '_' {
                upper = true;
            } else if upper {
                s.push(c.to_ascii_uppercase());
                upper = false;
            } else {
                s.push(c);
            }
        }
        s.push_str("VmController");
        s
    };

    fs::create_dir_all(dir.join("src"))?;
    fs::write(
        dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"{package}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
             license = \"MIT\"\n\
             description = \"Aseman VM plugin: {key} runtime\"\n\n\
             [lib]\nname = \"{lib_name}\"\npath = \"src/lib.rs\"\n\n\
             [dependencies]\naseman-config = {{ path = \"../../../crates/aseman-config\" }}\n\
             aseman-vm-sdk = {{ path = \"../sdk\" }}\nserde_json = \"1\"\n"
        ),
    )?;
    fs::write(
        dir.join("vm.config.json"),
        format!(
            "{{\n  \"key\": \"{key}\",\n  \"name\": \"{key} VM\",\n  \"version\": \"0.1.0\",\n  \
             \"description\": \"\",\n  \"aliases\": [],\n  \"artifactExtensions\": [],\n  \
             \"inProcess\": true,\n  \"defaultRuntime\": false,\n  \
             \"entityFileName\": \"module.{key}\",\n  \"acceptsExtraFiles\": false,\n  \
             \"buildOnDeploy\": false,\n  \"setEntityLinksOnDeploy\": false,\n  \
             \"supportsChainTrxs\": false,\n  \"restorable\": false\n}}\n"
        ),
    )?;
    fs::write(
        dir.join("src/lib.rs"),
        format!(
            "//! Aseman VM plugin: {key} runtime.\n\n\
             mod controller;\n\n\
             use std::sync::Arc;\n\n\
             use aseman_vm_sdk::{{registry, VmPluginMeta}};\n\n\
             pub use controller::{struct_name};\n\n\
             /// Register this VM type with the native backend's plugin registry, configured\n\
             /// from `config`. Invoked by the build-time-generated plugin aggregation crate.\n\
             pub fn register(_config: &aseman_config::RuntimeConfig) {{\n    \
             let meta = VmPluginMeta::from_config_str(include_str!(\"../vm.config.json\"))\n        \
             .expect(\"{package}: invalid vm.config.json\");\n    \
             registry::register_plugin(Arc::new({struct_name}::new(meta)));\n}}\n"
        ),
    )?;
    fs::write(
        dir.join("src/controller.rs"),
        format!(
            "//! The {key} VM controller — implement the full VM lifecycle here.\n\n\
             use serde_json::{{json, Value as JsonValue}};\n\n\
             use aseman_vm_sdk::{{VmPlugin, VmPluginMeta}};\n\n\
             pub struct {struct_name} {{\n    meta: VmPluginMeta,\n}}\n\n\
             impl {struct_name} {{\n    pub fn new(meta: VmPluginMeta) -> Self {{\n        Self {{ meta }}\n    }}\n}}\n\n\
             impl VmPlugin for {struct_name} {{\n    \
             fn meta(&self) -> &VmPluginMeta {{\n        &self.meta\n    }}\n\n    \
             fn run_vm(&self, packet: &JsonValue) -> Result<JsonValue, String> {{\n        \
             let machine_id = packet[\"machineId\"].as_str().unwrap_or(\"\");\n        \
             if machine_id.is_empty() {{\n            return Err(\"machineId is required\".to_string());\n        }}\n        \
             // TODO: launch the VM. Reach the node through aseman_vm_sdk::host().\n        \
             Ok(json!({{\"ok\": true, \"runtime\": \"{key}\", \"machineId\": machine_id}}))\n    }}\n\n    \
             fn terminate_vm(&self, packet: &JsonValue) -> Result<JsonValue, String> {{\n        \
             let machine_id = packet[\"machineId\"].as_str().unwrap_or(\"\");\n        \
             if machine_id.is_empty() {{\n            return Err(\"machineId is required\".to_string());\n        }}\n        \
             // TODO: stop the VM.\n        \
             Ok(json!({{\"ok\": true, \"runtime\": \"{key}\", \"machineId\": machine_id}}))\n    }}\n}}\n"
        ),
    )?;

    println!(
        "scaffolded VM plugin project at {}\n\
         Next steps:\n  \
         1. implement src/controller.rs against the aseman-vm-sdk traits\n  \
         2. fill in vm.config.json (aliases, deploy behaviour, ...)\n  \
         3. `asemanctl vms list` to verify discovery\n  \
         4. `asemanctl vms sync`, then rebuild the native VMM backend",
        dir.display()
    );
    Ok(())
}
