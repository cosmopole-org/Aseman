//! The A504 transport (ADR 0029): [`server::BackendService`] serves any
//! [`VmmBackend`](aseman_ports::vmm::VmmBackend) over gRPC, and [`client::GrpcBackend`]
//! is that port on the VMM service's side. Plaintext gRPC is accepted on loopback
//! addresses only: the service and its backend run on the same host.
#![forbid(unsafe_code)]

pub mod client;
pub mod convert;
pub mod server;
