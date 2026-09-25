//! Federation HTTP ownership boundary.
//!
//! The provider owns both the durable directory/replay state and the HTTP translation
//! into the destination-side federation use case. It never calls legacy node handlers.
#![forbid(unsafe_code)]

pub mod client;
pub mod http;
pub mod store;

pub use client::{
    FederationClientConfig, FederationClientError, FederationProofSigner,
    FederationResponseVerifier, FederationTls, HttpFederationClient,
};
pub use http::{
    FederationExecutor, FederationHttpConfig, FederationHttpError, FederationHttpHandler,
    FederationResponseSigner, FederationServerTls, FederationService, SignedFederationResponse,
    router, serve, tls_config,
};
pub use store::PostgresFederation;

/// Idempotent schema migration owned by the federation provider.
pub const FEDERATION_MIGRATION: &str = include_str!("../migrations/0001_federation.sql");
