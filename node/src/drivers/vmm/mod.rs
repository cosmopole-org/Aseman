//! Translation of `drivers/vmm` — the virtual-machine driver.
//!
//! Monolithic in-process VM driver with a plugin-based runtime system.
//!
//! VM control and host calls are routed as in-memory JSON packets through
//! [`dispatch_packet`] and handled by the bridge packet router, which resolves
//! the responsible VM runtime dynamically through the `caspar_vm_sdk` plugin
//! registry. The VM runtime implementations themselves live in standalone
//! plugin projects under the repository's `vms/` folder; they are compiled
//! into the node by the build-time-generated `caspar-vm-plugins` crate and
//! reach the node exclusively through the SDK's `VmHost` interface,
//! implemented in [`host_bridge`].

pub mod bootstrap;
pub mod bridge;
pub mod driver;
pub mod globals;
pub mod host;
pub mod host_bridge;
pub mod hostcall_entities;
pub mod hostcall_global;
pub mod hostcall_logs;
pub mod http_route;
pub mod network;
mod prelude;
pub mod proxy;

use serde_json::Value as JsonValue;

pub use driver::Vmm;
pub use host_bridge::init_vm_plugins;

pub fn dispatch_packet(packet: &JsonValue) -> String {
    bridge::vm_packet_router::route_vm_packet(packet)
}
