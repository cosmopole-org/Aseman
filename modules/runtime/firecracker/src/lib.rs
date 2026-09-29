//! Aseman VM plugin: Firecracker microVM runtime (`fire`).

mod controller;
pub mod models;

use std::sync::Arc;

use aseman_vm_sdk::{registry, VmPluginMeta};

pub use controller::{FireSettings, FireVmController};

/// Register this VM type with the Aseman VMM plugin registry.
/// Invoked by the build-time-generated plugin aggregation crate.
pub fn register(config: &aseman_config::RuntimeConfig) {
    let meta = VmPluginMeta::from_config_str(include_str!("../vm.config.json"))
        .expect("aseman-vm-fire: invalid vm.config.json");
    registry::register_plugin(Arc::new(FireVmController::new(
        meta,
        FireSettings::from_config(config),
    )));
}
