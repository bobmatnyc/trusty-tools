//! File-level helpers for the token stores: read, write, lock, and identity.
//!
//! Why: Split out of `storage/mod.rs` to keep it under the 500-SLOC cap, and
//! so the error type that must never echo file content (#8539) sits in one
//! place.
//! What: [`read_store`] with a content-free [`StoreReadError`], [`write_store`]
//! (0600 on Unix), [`open_lock`] for the sidecar lock file, and
//! [`same_store`] to detect two paths naming one file.
//! Test: `tests` below; `storage/tests.rs` covers the callers.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::api::auth::models::StoredToken;

/// Why a store file could not be read.
///
/// Why: serde's `Display` can quote a value from the file, and a token store
/// holds credentials (#8539). This type keeps only the path, the I/O error
/// kind, or serde's position and category.
/// What: `Display` prints nothing that came from the file's bytes.
/// Test: `load_warns_on_unparsable_store_without_echoing_it`.
#[derive(Debug)]
pub(super) enum StoreReadError {
    Io {
        path: PathBuf,
        kind: std::io::ErrorKind,
    },
    Parse {
        path: PathBuf,
        line: usize,
        column: usize,
        category: serde_json::error::Category,
    },
}

impl StoreReadError {
    pub(super) fn path(&self) -> &Path {
        match self {
            StoreReadError::Io { path, .. } | StoreReadError::Parse { path, .. } => path,
        }
    }
}

impl fmt::Display for StoreReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreReadError::Io { path, kind } => {
                write!(f, "cannot read token store {}: {kind}", path.display())
            }
            StoreReadError::Parse {
                path,
                line,
                column,
                category,
            } => write!(
                f,
                "cannot parse token store {}: {category:?} error at line {line} column {column}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for StoreReadError {}

/// Read one store; a missing file is an empty store.
pub(super) fn read_store(
    path: &Path,
) -> std::result::Result<HashMap<String, StoredToken>, StoreReadError> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let data = std::fs::read_to_string(path).map_err(|e| StoreReadError::Io {
        path: path.to_path_buf(),
        kind: e.kind(),
    })?;
    serde_json::from_str(&data).map_err(|e| StoreReadError::Parse {
        path: path.to_path_buf(),
        line: e.line(),
        column: e.column(),
        category: e.classify(),
    })
}

/// Write one store file: pretty JSON, then (Unix) mode 0600.
///
/// Why: `tokens.json` holds live OAuth refresh tokens; owner-only mode keeps
/// other local users from reading it.
/// What: Creates the parent directory, writes, then restricts permissions.
/// Test: `save_restricts_permissions_on_unix`.
pub(super) fn write_store(target: &Path, tokens: &HashMap<String, StoredToken>) -> Result<()> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let data = serde_json::to_string_pretty(tokens)?;
    std::fs::write(target, data)
        .with_context(|| format!("write tokens to {}", target.display()))?;
    restrict_permissions(target)
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 0600 {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// Open the sidecar `<store>.lock` file guarding `store`; it never holds
/// token data.
pub(super) fn open_lock(store: &Path) -> Result<fd_lock::RwLock<std::fs::File>> {
    let file_name = store
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tokens.json".to_string());
    let lock_path = store.with_file_name(format!("{file_name}.lock"));
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open lock file {}", lock_path.display()))?;
    Ok(fd_lock::RwLock::new(file))
}

/// Whether `a` and `b` name the same store file.
///
/// Why: With cwd = `$HOME`, or a symlinked `.gworkspace-mcp`, the project
/// and user paths are one file. Locking it twice through two `open()`s
/// self-deadlocks, since `flock` treats each open file description as a
/// separate lock (#8539).
/// What: Compares the paths after canonicalizing each one, or its parent
/// directory when the file itself does not exist yet.
/// Test: `same_store_sees_through_a_symlinked_dir`,
/// `update_does_not_deadlock_when_project_and_user_are_the_same_file`.
pub(super) fn same_store(a: &Path, b: &Path) -> bool {
    a == b || resolved(a) == resolved(b)
}

fn resolved(path: &Path) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(dir), Some(name)) => dir
            .canonicalize()
            .map(|d| d.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn same_store_sees_through_a_symlinked_dir() {
        let root = std::env::temp_dir().join(format!("gw-same-{}", uuid::Uuid::new_v4()));
        let real = root.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = root.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert!(same_store(
            &real.join("tokens.json"),
            &link.join("tokens.json")
        ));
        assert!(!same_store(
            &real.join("tokens.json"),
            &root.join("other").join("tokens.json")
        ));
    }
}
