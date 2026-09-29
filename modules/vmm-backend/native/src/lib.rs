//! The native-legacy VMM backend (P5-03, ADR 0029): the Caspar runtime plugins,
//! extracted from the node, behind A504. The plugins reach the node only through the
//! authenticated guest API, as the workload they run.
#![forbid(unsafe_code)]

pub mod backend;
mod docker_host;
pub mod host;
pub mod registry;
