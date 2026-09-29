//! Per-VM sandbox directories under a runtime's storage root: `{root}/vms/<vm>`.
//!
//! Every path handed to a VM is resolved here, and canonicalised and checked to lie
//! strictly inside the sandbox root, so no VM id can name a directory outside it.

use std::path::{Path, PathBuf};

/// The storage root when none is configured: the in-container default, else a local
/// development path.
#[must_use]
pub fn default_storage_root() -> PathBuf {
    let in_container = PathBuf::from("/app/data/storage");
    if in_container.exists() {
        return in_container;
    }
    PathBuf::from("/tmp/caspar/storage")
}

/// The sandbox root of one runtime: `{storage_root}/vms`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmSandbox {
    root: PathBuf,
}

impl VmSandbox {
    /// The sandbox under `storage_root`.
    #[must_use]
    pub fn under(storage_root: &Path) -> Self {
        Self {
            root: storage_root.join("vms"),
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The deterministic directory of `vm_id` (no side effects).
    #[must_use]
    pub fn vm_dir(&self, vm_id: &str) -> PathBuf {
        self.root.join(sanitize_component(if vm_id.is_empty() {
            "main"
        } else {
            vm_id
        }))
    }

    /// Create the directory of `vm_id` and return its canonical path, refusing one
    /// that resolves outside the sandbox root.
    ///
    /// # Errors
    ///
    /// The directory cannot be created or canonicalised, or escapes the root.
    pub fn session_dir(&self, vm_id: &str) -> Result<PathBuf, String> {
        std::fs::create_dir_all(&self.root)
            .map_err(|e| format!("failed to prepare vms root {}: {}", self.root.display(), e))?;
        let canon_root = std::fs::canonicalize(&self.root)
            .map_err(|e| format!("failed to canonicalize vms root: {}", e))?;
        let dir = self.vm_dir(vm_id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("failed to create session vm dir {}: {}", dir.display(), e))?;
        let canon_dir = std::fs::canonicalize(&dir)
            .map_err(|e| format!("failed to canonicalize session vm dir: {}", e))?;
        if !canon_dir.starts_with(&canon_root) || canon_dir == canon_root {
            return Err(format!(
                "refusing to use vm dir outside sandbox root: {}",
                canon_dir.display()
            ));
        }
        Ok(canon_dir)
    }

    /// Whether `dir` lies strictly inside the sandbox root (the guard before a
    /// destructive purge).
    #[must_use]
    pub fn contains(&self, dir: &Path) -> bool {
        let canon_root = std::fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        match std::fs::canonicalize(dir) {
            Ok(c) => c.starts_with(&canon_root) && c != canon_root,
            Err(_) => dir.starts_with(&canon_root) && dir != canon_root,
        }
    }
}

/// Reduce a caller-supplied id to a single safe path component.
fn sanitize_component(raw: &str) -> String {
    let out: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_cannot_leave_the_sandbox() {
        let root = std::env::temp_dir().join(format!("vm-sandbox-{}", std::process::id()));
        let sandbox = VmSandbox::under(&root);
        assert_eq!(sandbox.vm_dir("../etc"), root.join("vms").join("___etc"));
        assert_eq!(sandbox.vm_dir(""), root.join("vms").join("main"));
        let dir = sandbox.session_dir("a/b").unwrap();
        assert!(sandbox.contains(&dir));
        assert!(!sandbox.contains(&root));
        assert!(!sandbox.contains(sandbox.root()));
        std::fs::remove_dir_all(root).unwrap();
    }
}
