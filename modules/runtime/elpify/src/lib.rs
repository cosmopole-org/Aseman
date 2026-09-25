//! Caspar VM plugin: Elpify provable (MASM/STARK) runtime (`elpify`).

mod controller;
mod queue;

use std::sync::Arc;

use caspar_vm_sdk::{registry, VmPluginMeta};

pub use controller::ElpifyVmController;

/// Register this VM type with the Caspar VMM plugin registry.
/// Invoked by the build-time-generated plugin aggregation crate.
pub fn register() {
    let meta = VmPluginMeta::from_config_str(include_str!("../vm.config.json"))
        .expect("caspar-vm-elpify: invalid vm.config.json");
    registry::register_plugin(Arc::new(ElpifyVmController::new(meta)));
}
