//! Aseman VM plugin: JavaScript runtime (`javascript`).
//!
//! A creature written in JavaScript is one self-contained bundle deployed as
//! the entity `module.js`. It runs in-process on QuickJS and reaches the
//! platform through exactly one import — `hostCall` — which speaks the same
//! `{op, input}` protocol, with the same op table, as the wasm runtime's.

mod controller;
mod host_calls;
mod runtime;

#[cfg(test)]
mod tests;

use std::sync::Arc;

use aseman_vm_sdk::{registry, VmPluginMeta};

pub use controller::JavascriptVmController;

/// Register this VM type with the Aseman VMM plugin registry.
/// Invoked by the build-time-generated plugin aggregation crate.
/// This runtime reads no runtime configuration.
pub fn register(_config: &aseman_config::RuntimeConfig) {
    let meta = VmPluginMeta::from_config_str(include_str!("../vm.config.json"))
        .expect("aseman-vm-javascript: invalid vm.config.json");
    registry::register_plugin(Arc::new(JavascriptVmController::new(meta)));
}
