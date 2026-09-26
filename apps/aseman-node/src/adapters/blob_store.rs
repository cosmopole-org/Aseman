//! The Phase 3 blob provider (ADR 0027): file bytes under the node's storage root,
//! exactly where legacy keeps them, behind the [`BlobStore`] port.

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use aseman_domain::blob::{BlobEvidence, valid_blob_key};
use aseman_ports::{BlobStore, PortError, PortResult};
use sha2::{Digest, Sha256};

/// The node's blob store: its storage root.
pub(crate) fn node_blobs(
    storage: &dyn crate::models::ports::storage::IStorage,
) -> StorageRootBlobStore {
    StorageRootBlobStore::new(storage.storage_root())
}

/// The public upload folder (`/storage/*` and `/creatures/storageUpload`).
pub(crate) const PUBLIC_FILES: &str = "public-files";

/// Blobs as files under one root directory; a key is the path relative to it.
pub(crate) struct StorageRootBlobStore {
    root: PathBuf,
}

impl StorageRootBlobStore {
    pub(crate) fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Store one deployed entity file under `machines/{program}/entities/{entity}`.
    /// The name comes from the deploy request, so it must not leave that folder
    /// (LD-25).
    pub(crate) fn put_entity_file(
        &self,
        program_id: &str,
        entity_id: &str,
        name: &str,
        bytes: &[u8],
    ) -> anyhow::Result<BlobEvidence> {
        let key = ["machines/", program_id, "/entities/", entity_id, "/", name].concat();
        self.put_blob(&key, bytes, "application/octet-stream", true)
            .map_err(|error| anyhow::anyhow!("entity file {name:?}: {error}"))
    }

    fn path(&self, key: &str) -> PortResult<PathBuf> {
        if !valid_blob_key(key) {
            return Err(PortError::Failed(format!("invalid blob key {key:?}")));
        }
        Ok(self.root.join(key))
    }

    /// The key of a legacy absolute path under this root, if it is one.
    pub(crate) fn key_of(&self, path: &str) -> Option<String> {
        let relative = std::path::Path::new(path).strip_prefix(&self.root).ok()?;
        let key = relative.to_str()?.to_owned();
        valid_blob_key(&key).then_some(key)
    }
}

fn failed(error: impl ToString) -> PortError {
    PortError::Failed(error.to_string())
}

impl BlobStore for StorageRootBlobStore {
    fn put_blob(
        &self,
        key: &str,
        bytes: &[u8],
        media_type: &str,
        overwrite: bool,
    ) -> PortResult<BlobEvidence> {
        let path = self.path(key)?;
        if !overwrite && path.exists() {
            return Err(PortError::Conflict);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(failed)?;
        }
        fs::write(&path, bytes).map_err(failed)?;
        Ok(BlobEvidence {
            store_key: key.to_owned(),
            content_digest: Sha256::digest(bytes).into(),
            size_bytes: bytes.len() as u64,
            media_type: if media_type.is_empty() {
                "application/octet-stream".to_owned()
            } else {
                media_type.to_owned()
            },
        })
    }

    fn blob(&self, key: &str) -> PortResult<Option<Vec<u8>>> {
        match fs::read(self.path(key)?) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(failed(error)),
        }
    }

    fn has_blob(&self, key: &str) -> PortResult<bool> {
        Ok(self.path(key)?.is_file())
    }

    fn delete_blob(&self, key: &str) -> PortResult<()> {
        match fs::remove_file(self.path(key)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(failed(error)),
        }
    }

    fn local_path(&self, key: &str) -> PortResult<PathBuf> {
        self.path(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_root_blobs_pass_the_blob_conformance_suite() {
        let root = std::env::temp_dir().join(format!(
            "aseman-blobs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let blobs = StorageRootBlobStore::new(&root);
        aseman_ports::conformance::blob_store(&blobs);
        // Legacy absolute paths under the root map to their keys.
        let legacy = root.join("machines/1@g/entities/main/module.wasm");
        assert_eq!(
            blobs.key_of(legacy.to_str().unwrap()).as_deref(),
            Some("machines/1@g/entities/main/module.wasm")
        );
        assert_eq!(blobs.key_of("/elsewhere/module.wasm"), None);
        // LD-25: a deploy file name cannot escape its entity folder.
        let evidence = blobs
            .put_entity_file("1@g", "main", "lib/util.js", b"x")
            .unwrap();
        assert_eq!(evidence.store_key, "machines/1@g/entities/main/lib/util.js");
        for name in ["../escape", "../../../../etc/x", "/abs", "a/../../b", ""] {
            assert!(
                blobs.put_entity_file("1@g", "main", name, b"x").is_err(),
                "{name}"
            );
        }
        assert!(!root.join("machines/1@g/escape").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
