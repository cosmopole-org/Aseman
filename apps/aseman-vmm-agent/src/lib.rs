//! `aseman-vmm-agent`: the one component with host privilege (A603, ADR 0010).
//!
//! It owns `/dev/kvm`, tap devices, cgroups, the jailer, and microVM process
//! lifecycle, so that the node, the VMM service, and the VMM backend do not have to.
//! Everything it will do is decided by `aseman-domain::agent`; this crate is the part
//! that touches the host.
#![forbid(unsafe_code)]

pub mod firecracker;
pub mod host;
