//! Shared RocksDB memory tuning now lives in the legacy storage provider
//! (`aseman_storage_legacy::tuning`). This re-export keeps one process-wide block
//! cache for the remaining in-node RocksDB users: the OpenRaft store (RL-012) and the
//! Hashgraph store (RL-011).

pub use aseman_storage_legacy::tuning::tuned_options;
