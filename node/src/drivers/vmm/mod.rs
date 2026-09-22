//! The node's side of workloads (P5-06, ADR 0030).
//!
//! Workloads run on the node's VMM (see `shell::workloads`); runtimes and their
//! plugins are the VMM backend's. What stays here is the node's:
//! - the guest host calls, served for workloads through the guest API
//!   ([`host`], [`guest_state`], [`hostcall_entities`]);
//! - signal delivery to programs, proxy entities, and alarms ([`driver`],
//!   [`proxy`]);
//! - HTTP ingress and custom gateway routes ([`network`], [`http_route`]).

pub mod driver;
pub mod globals;
pub(crate) mod guest_state;
pub mod host;
pub mod hostcall_entities;
pub mod http_route;
pub mod network;
mod prelude;
pub mod proxy;

pub use driver::NodeWorkloads;
