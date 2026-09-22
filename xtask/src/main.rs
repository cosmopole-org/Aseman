//! Repository task runner.
#![forbid(unsafe_code)]

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn main() -> Result<()> {
    let root = workspace_root()?;
    match env::args().nth(1).as_deref() {
        Some("arch") => check_architecture(&root),
        Some("fast") => fast(&root),
        Some("full") => {
            fast(&root)?;
            full(&root)
        }
        _ => bail!("usage: cargo xtask <arch|fast|full>"),
    }
}

fn workspace_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .context("xtask must be inside workspace")
}

fn run(root: &Path, program: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("failed to start {program}"))?;
    if !status.success() {
        bail!("{program} {} failed with {status}", args.join(" "));
    }
    Ok(())
}

fn metadata(root: &Path) -> Result<Value> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(root)
        .output()
        .context("cargo metadata")?;
    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).context("parse cargo metadata")
}

fn check_architecture(root: &Path) -> Result<()> {
    let data = metadata(root)?;
    let packages = data["packages"].as_array().context("metadata packages")?;
    let names: BTreeSet<&str> = packages.iter().filter_map(|p| p["name"].as_str()).collect();
    for required in [
        "aseman-domain",
        "aseman-ports",
        "aseman-application",
        "aseman-contracts",
        "aseman-config",
    ] {
        if !names.contains(required) {
            bail!("missing required architecture package {required}");
        }
    }
    let allowed: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::from([
        (
            "aseman-domain",
            BTreeSet::from(["serde", "thiserror", "uuid"]),
        ),
        (
            "aseman-ports",
            BTreeSet::from(["aseman-domain", "thiserror"]),
        ),
        (
            "aseman-application",
            BTreeSet::from(["aseman-domain", "aseman-ports", "thiserror"]),
        ),
    ]);
    for package in packages {
        let Some(name) = package["name"].as_str() else {
            continue;
        };
        let Some(package_allowed) = allowed.get(name) else {
            continue;
        };
        for dependency in package["dependencies"]
            .as_array()
            .context("package dependencies")?
        {
            let dependency_name = dependency["name"].as_str().context("dependency name")?;
            if !package_allowed.contains(dependency_name) {
                bail!("forbidden dependency: {name} -> {dependency_name}");
            }
        }
    }
    println!("architecture dependency rules passed");
    Ok(())
}

fn fast(root: &Path) -> Result<()> {
    check_architecture(root)?;
    for package in [
        "aseman-domain",
        "aseman-ports",
        "aseman-application",
        "aseman-contracts",
        "aseman-config",
        "aseman-observability",
        "aseman-module-runtime",
        "aseman-sample-provider",
        "aseman-module-conformance",
        "aseman-storage-conformance",
        "aseman-storage-postgres",
        "aseman-storage-legacy",
        "aseman-migration-e2e",
        "aseman-capsule-repositories",
        "aseman-identity-native",
        "aseman-policy-native",
        "aseman-policy-conformance",
        "xtask",
    ] {
        run(root, "cargo", &["fmt", "-p", package, "--", "--check"])?;
    }
    for script in [
        "generate_current_workspace_inventory.py",
        "generate_current_surface_inventories.py",
        "generate_legacy_data_inventory.py",
        "generate_current_call_graph.py",
        "generate_characterization_fixtures.py",
        "generate_support_manifest.py",
        "generate_quality_baseline.py",
        "generate_removal_ledger_children.py",
        "generate_phase1_contracts.py",
        "generate_phase2_contracts.py",
        "generate_phase3_contracts.py",
        "generate_postgres_core.py",
        "generate_postgres_storage_classes.py",
        "generate_legacy_transform_manifest.py",
        "generate_security_registry.py",
        "generate_vmm_parity.py",
    ] {
        run(root, "python3", &[&format!("scripts/{script}"), "--check"])?;
    }
    run(
        root,
        "python3",
        &[
            "-m",
            "unittest",
            "discover",
            "-s",
            "tests/characterization",
            "-p",
            "test_*.py",
        ],
    )?;
    run(
        root,
        "cargo",
        &[
            "test",
            "-p",
            "aseman-domain",
            "-p",
            "aseman-ports",
            "-p",
            "aseman-application",
            "-p",
            "aseman-contracts",
            "-p",
            "aseman-config",
            "-p",
            "aseman-observability",
            "-p",
            "aseman-module-runtime",
            "-p",
            "aseman-sample-provider",
            "-p",
            "aseman-module-conformance",
            "-p",
            "aseman-storage-conformance",
            "-p",
            "aseman-storage-postgres",
            "-p",
            "aseman-storage-legacy",
            "-p",
            "aseman-migration-e2e",
            "-p",
            "aseman-capsule-repositories",
            "-p",
            "aseman-identity-native",
            "-p",
            "aseman-policy-native",
            "-p",
            "aseman-policy-conformance",
            "-p",
            "aseman-vmm-http",
            "-p",
            "aseman-vmm-backend-grpc",
            "-p",
            "aseman-vmm-backend-conformance",
            "-p",
            "aseman-guest-http",
            "-p",
            "aseman-vmm",
        ],
    )?;
    run(
        root,
        "cargo",
        &[
            "clippy",
            "-p",
            "aseman-domain",
            "-p",
            "aseman-ports",
            "-p",
            "aseman-application",
            "-p",
            "aseman-contracts",
            "-p",
            "aseman-config",
            "-p",
            "aseman-observability",
            "-p",
            "aseman-module-runtime",
            "-p",
            "aseman-sample-provider",
            "-p",
            "aseman-module-conformance",
            "-p",
            "aseman-storage-conformance",
            "-p",
            "aseman-storage-postgres",
            "-p",
            "aseman-storage-legacy",
            "-p",
            "aseman-migration-e2e",
            "-p",
            "aseman-capsule-repositories",
            "-p",
            "aseman-identity-native",
            "-p",
            "aseman-policy-native",
            "-p",
            "aseman-policy-conformance",
            "-p",
            "aseman-vmm-http",
            "-p",
            "aseman-vmm-backend-grpc",
            "-p",
            "aseman-vmm-backend-conformance",
            "-p",
            "aseman-guest-http",
            "-p",
            "aseman-vmm",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )
}

fn full(root: &Path) -> Result<()> {
    run(root, "cargo", &["check", "-p", "caspar-node", "--bins"])?;
    run(root, "cargo", &["test", "-p", "caspar-node", "--lib"])?;
    // The native backend links every runtime plugin, as the node does.
    run(root, "cargo", &["test", "-p", "aseman-vmm-backend-native"])?;
    run(
        root,
        "cargo",
        &[
            "clippy",
            "-p",
            "aseman-vmm-backend-native",
            "--all-targets",
            "--no-deps",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    run(root, "cargo", &["test", "-p", "casparctl"])
}
