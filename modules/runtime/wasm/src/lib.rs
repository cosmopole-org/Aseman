//! Caspar VM plugin: WasmEdge-managed WebAssembly runtime (`wasm`).
//!
//! This is the platform's default runtime: run requests whose hints and
//! artifact paths match no other registered VM type fall back to it.

mod controller;
pub mod host_calls;
pub mod models;
pub mod runtime;

use std::sync::Arc;

use caspar_vm_sdk::{registry, VmPluginMeta};

pub use controller::WasmVmController;
pub use runtime::terminate_managed_vm;

/// Register this VM type with the Caspar VMM plugin registry.
/// Invoked by the build-time-generated plugin aggregation crate.
pub fn register() {
    let meta = VmPluginMeta::from_config_str(include_str!("../vm.config.json"))
        .expect("caspar-vm-wasm: invalid vm.config.json");
    registry::register_plugin(Arc::new(WasmVmController::new(meta)));
}
