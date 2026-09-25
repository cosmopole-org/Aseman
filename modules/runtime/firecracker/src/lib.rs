//! Caspar VM plugin: Firecracker microVM runtime (`fire`).

mod controller;
pub mod models;

use std::sync::Arc;

use caspar_vm_sdk::{registry, VmPluginMeta};

pub use controller::FireVmController;

/// Register this VM type with the Caspar VMM plugin registry.
/// Invoked by the build-time-generated plugin aggregation crate.
pub fn register() {
    let meta = VmPluginMeta::from_config_str(include_str!("../vm.config.json"))
        .expect("caspar-vm-fire: invalid vm.config.json");
    registry::register_plugin(Arc::new(FireVmController::new(meta)));
}
