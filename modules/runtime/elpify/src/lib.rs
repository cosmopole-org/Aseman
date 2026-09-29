//! Aseman VM plugin: Elpify provable (MASM/STARK) runtime (`elpify`).

mod controller;
mod queue;

use std::sync::Arc;

use aseman_vm_sdk::{registry, VmPluginMeta};

pub use controller::ElpifyVmController;

/// Register this VM type with the Aseman VMM plugin registry.
/// Invoked by the build-time-generated plugin aggregation crate.
/// This runtime reads no runtime configuration.
pub fn register(_config: &aseman_config::RuntimeConfig) {
    let meta = VmPluginMeta::from_config_str(include_str!("../vm.config.json"))
        .expect("aseman-vm-elpify: invalid vm.config.json");
    registry::register_plugin(Arc::new(ElpifyVmController::new(meta)));
}
