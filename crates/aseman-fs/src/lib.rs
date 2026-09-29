//! Durable, atomic file replacement.
//!
//! [`write_atomic`] writes the new contents to a temporary file beside the target,
//! syncs it, renames it over the target, and syncs the directory, so a reader sees
//! either the old file or the whole new one and a crash never leaves a torn or
//! empty file. The temporary file is created with its final permissions: a private
//! file is never readable by others, not even for an instant.

use std::fs::{File, Permissions};
use std::io::{self, Write};
use std::path::Path;

/// Who may read the written file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Access {
    /// Owner read/write only (`0600`): secrets, keys, and state holding them.
    Private,
    /// The process's default for new files (`0666` less the umask).
    Shared,
}

#[cfg(unix)]
fn permissions(access: Access) -> Permissions {
    use std::os::unix::fs::PermissionsExt;
    Permissions::from_mode(match access {
        Access::Private => 0o600,
        Access::Shared => 0o666,
    })
}

/// Replace `path` with `contents` atomically and durably.
///
/// The parent directory must exist.
///
/// # Errors
///
/// Any failure creating, writing, syncing, or renaming the file, or syncing its
/// directory. On failure the target is unchanged and the temporary file is removed.
pub fn write_atomic(path: &Path, contents: &[u8], access: Access) -> io::Result<()> {
    let (directory, file) = staged(path, contents, access)?;
    file.persist(path).map_err(|error| error.error)?;
    sync_directory(directory)
}

/// Create `path` with `contents` atomically and durably, never replacing an
/// existing file: concurrent creators of the same path see exactly one win.
///
/// The parent directory must exist.
///
/// # Errors
///
/// `AlreadyExists` when `path` exists, and any failure [`write_atomic`] reports.
pub fn create_atomic(path: &Path, contents: &[u8], access: Access) -> io::Result<()> {
    let (directory, file) = staged(path, contents, access)?;
    file.persist_noclobber(path).map_err(|error| error.error)?;
    sync_directory(directory)
}

/// `contents` written and synced to a temporary file in `path`'s directory.
fn staged<'a>(
    path: &'a Path,
    contents: &[u8],
    access: Access,
) -> io::Result<(&'a Path, tempfile::NamedTempFile)> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut builder = tempfile::Builder::new();
    builder.prefix(".").suffix(".tmp");
    #[cfg(unix)]
    builder.permissions(permissions(access));
    #[cfg(not(unix))]
    let _ = access;
    let mut file = builder.tempfile_in(directory)?;
    file.write_all(contents)?;
    file.as_file().sync_all()?;
    Ok((directory, file))
}

/// Make a rename in `directory` durable.
#[cfg(unix)]
fn sync_directory(directory: &Path) -> io::Result<()> {
    File::open(directory)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "aseman-fs-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos())
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn replaces_the_file_and_leaves_no_temporary_behind() {
        let directory = scratch("replace");
        let path = directory.join("state.json");
        write_atomic(&path, b"first", Access::Shared).unwrap();
        write_atomic(&path, b"second", Access::Shared).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        let entries: Vec<_> = std::fs::read_dir(&directory).unwrap().collect();
        assert_eq!(entries.len(), 1, "only the target remains");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let directory = scratch("private");
        let path = directory.join("secret");
        write_atomic(&path, b"key", Access::Private).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn create_never_replaces_an_existing_file() {
        let directory = scratch("create");
        let path = directory.join("key");
        create_atomic(&path, b"first", Access::Private).unwrap();
        let error = create_atomic(&path, b"second", Access::Private).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        let entries: Vec<_> = std::fs::read_dir(&directory).unwrap().collect();
        assert_eq!(entries.len(), 1, "the losing temporary is removed");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_missing_directory_fails_and_writes_nothing() {
        let directory = scratch("missing");
        let path = directory.join("absent").join("file");
        assert!(write_atomic(&path, b"x", Access::Shared).is_err());
        assert!(!path.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
