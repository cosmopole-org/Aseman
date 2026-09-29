//! Federation HTTP ownership boundary.
//!
//! The provider owns the HTTP translation into the destination-side federation use
//! case; the directory and replay state are ports on the storage module (ADR 0038).
//! It never calls node handlers.
#![forbid(unsafe_code)]

pub mod client;
pub mod http;

pub use client::{
    DescriptorHttpTransport, FederationClientConfig, FederationClientError,
    FederationNodeCredential, FederationProofSigner, FederationResponseVerifier, FederationTls,
    HttpFederationClient, federation_audience,
};
pub use http::{
    FederationExecutor, FederationHttpConfig, FederationHttpError, FederationHttpHandler,
    FederationResponseSigner, FederationServerTls, FederationService, SignedFederationResponse,
    router, serve, tls_config,
};
