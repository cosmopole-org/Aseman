//! Wire-contract primitives shared by generated protocol packages.
#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

pub mod capsule;
pub mod guest;
pub mod identity;
pub mod legacy_documents;
pub mod legacy_gateway;
pub mod legacy_keys;
pub mod legacy_realtime;
pub mod legacy_storage_http;
pub mod migration;
pub mod module;
pub mod security;
pub mod vmm;

// Tonic's generated service signatures return its fixed `Status` error by value.
// This boundary code cannot change that ABI; application code maps it immediately.
#[allow(clippy::result_large_err)]
pub mod aseman {
    pub mod capsule {
        pub mod provider {
            pub mod v1 {
                tonic::include_proto!("aseman.capsule.provider.v1");
            }
        }
    }
    pub mod module {
        pub mod control {
            pub mod v1 {
                tonic::include_proto!("aseman.module.control.v1");
            }
        }
        pub mod provider {
            pub mod v1 {
                tonic::include_proto!("aseman.module.provider.v1");
            }
        }
        pub mod sample {
            pub mod v1 {
                tonic::include_proto!("aseman.module.sample.v1");
            }
        }
    }
}

pub use aseman::capsule::provider::v1 as capsule_provider_v1;
pub use aseman::module::control::v1 as module_control_v1;
pub use aseman::module::provider::v1 as module_provider_v1;
pub use aseman::module::sample::v1 as module_sample_v1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContractVersion {
    pub major: u16,
    pub minor: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub code: String,
    pub message: String,
    pub request_id: String,
    pub retryable: bool,
}
