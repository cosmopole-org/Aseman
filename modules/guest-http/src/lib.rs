//! The guest API transport (A405, P5-04).
//!
//! [`server`] is the node's listener: it decodes the A401 proof header and hands the
//! request to the node's [`server::GuestApi`], which authenticates the workload and
//! serves the call as that workload. [`client`] is what a VMM backend uses to call
//! for a workload it runs, signing with the workload's credential.
#![forbid(unsafe_code)]

pub mod client;
pub mod server;
