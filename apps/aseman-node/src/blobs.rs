//! The blob provider (ADR 0027): file bytes under the node's storage root, at
//! their paths under the storage root, behind the [`BlobStore`] port. The
//! implementation is shared with the action plugins through `aseman-action-sdk`
//! (ADR 0040).

pub use aseman_action_sdk::blobs::{PUBLIC_FILES, StorageRootBlobStore};

/// The node's blob store: its storage root.
pub(crate) fn node_blobs(storage: &crate::storage::NodeStorage) -> StorageRootBlobStore {
    StorageRootBlobStore::new(storage.storage_root())
}