//! Agent-control items for the 0600 file backend (#10386, #10090).
//!
//! Why: Architect ruling Q5 (2026-10-28) — the file backend persists the
//! per-key agent deny, so an operator on a host with no Keychain can deny a
//! key to agents.
//! What: items live under `<root>/.agent-controls/<vault file stem>/`: a
//! deny is the empty-content 0600 file `deny/<encoded key>`, the vault's
//! migration marker is the file `migrated`. Directories are 0700. Every path
//! passes the backend's no-follow, type, mode and owner check
//! ([`super::file::judge`]); a refusal or an I/O fault is an error, never a
//! guess. The file backend never held the pre-#10386 opt-in flag (#10090), so
//! that item always reads absent and only its removal succeeds.
//! Test: `file_tests.rs` — `file_backend_persists_the_agent_deny`,
//! `file_backend_control_refuses_a_symlinked_deny`.

use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use super::ControlItem;
use super::file::{Kind, create_dir, file_name, judge, sync_dir};
use crate::api::{BackendId, SecretKey, SecretsError, VaultName};

/// The control directory under the backend root.
pub(crate) const CONTROLS_SUBDIR: &str = ".agent-controls";

/// The directory holding deny items, under a vault's control directory.
const DENY_DIR: &str = "deny";

/// The marker file, under a vault's control directory.
const MIGRATED_FILE: &str = "migrated";

/// The directory and file name `item` lives at, or `None` for an item the
/// file backend never stores.
fn location(
    root: &Path,
    item: ControlItem,
    vault: &VaultName,
    key: Option<&SecretKey>,
) -> Result<Option<(PathBuf, String)>, SecretsError> {
    let vault_dir = root.join(CONTROLS_SUBDIR).join(vault.file_stem());
    match (item, key) {
        (ControlItem::AgentDeny, Some(key)) => {
            Ok(Some((vault_dir.join(DENY_DIR), file_name(key)?)))
        }
        (ControlItem::DenyMigrated, None) => Ok(Some((vault_dir, MIGRATED_FILE.to_string()))),
        (ControlItem::LegacyAgentsFlag, _) => Ok(None),
        _ => Err(unsupported(item)),
    }
}

fn unsupported(item: ControlItem) -> SecretsError {
    SecretsError::Unsupported {
        backend: BackendId::FILE.to_string(),
        operation: item.operation(),
    }
}

/// Whether `item` is present; see the module docs.
pub(crate) fn read(
    root: &Path,
    item: ControlItem,
    vault: &VaultName,
    key: Option<&SecretKey>,
) -> Result<bool, SecretsError> {
    let Some((dir, name)) = location(root, item, vault, key)? else {
        return Ok(false);
    };
    let path = dir.join(name);
    match fs::symlink_metadata(&path) {
        Ok(meta) => judge(&path, &meta, Kind::File).map(|()| true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(SecretsError::Io { path, source }),
    }
}

/// Create or remove `item`; see the module docs.
pub(crate) fn write(
    root: &Path,
    item: ControlItem,
    vault: &VaultName,
    key: Option<&SecretKey>,
    present: bool,
) -> Result<(), SecretsError> {
    let Some((dir, name)) = location(root, item, vault, key)? else {
        // #10090: the file backend never held the opt-in flag.
        return if present {
            Err(unsupported(item))
        } else {
            Ok(())
        };
    };
    let path = dir.join(name);
    if present {
        create_dir(root, true)?;
        let mut parent = root.to_path_buf();
        for part in dir.strip_prefix(root).unwrap_or(&dir).components() {
            parent.push(part);
            create_dir(&parent, false)?;
        }
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(SecretsError::Io { path, source }),
        }
        let meta = fs::symlink_metadata(&path).map_err(|source| SecretsError::Io {
            path: path.clone(),
            source,
        })?;
        judge(&path, &meta, Kind::File)?;
        return sync_dir(&dir);
    }
    match fs::symlink_metadata(&path) {
        Ok(meta) => judge(&path, &meta, Kind::File)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(SecretsError::Io { path, source }),
    }
    match fs::remove_file(&path) {
        Ok(()) => sync_dir(&dir),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SecretsError::Io { path, source }),
    }
}
