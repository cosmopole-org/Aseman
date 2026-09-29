//! Aseman VM plugin: Modal cloud sandbox runtime (`modal`).
//!
//! Where the docker runtime supervises containers on the node's own machine
//! and fire boots microVMs on it, this runtime has no local process at all:
//! a VM is a **Modal sandbox** running in Modal's cloud, addressed over
//! Modal's gRPC control plane. Everything the platform expects of a runtime —
//! run, terminate, delete, status, exec, file transfer, inbound HTTP — is
//! implemented against that API, so a modal VM is an ordinary Aseman VM
//! everywhere else in the node.

mod client;
mod controller;
#[cfg(test)]
mod live;
mod models;
mod settings;

use std::sync::Arc;

use aseman_vm_sdk::{registry, VmPluginMeta};

pub use controller::ModalVmPlugin;
pub use settings::ModalSettings;

/// The generated Modal gRPC client, from the vendored proto slice.
pub mod proto {
    #![allow(clippy::all)]
    tonic::include_proto!("modal.client");
}

/// Register this VM type with the Aseman VMM plugin registry.
/// Invoked by the build-time-generated plugin aggregation crate.
pub fn register(config: &aseman_config::RuntimeConfig) {
    let meta = VmPluginMeta::from_config_str(include_str!("../vm.config.json"))
        .expect("aseman-vm-modal: invalid vm.config.json");
    registry::register_plugin(Arc::new(ModalVmPlugin::new(
        meta,
        ModalSettings::from_config(config),
    )));
}
