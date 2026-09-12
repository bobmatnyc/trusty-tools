//! Private atomic state with an advisory cross-process mutation lock.
use super::*;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};
const MAX_STATE_BYTES: u64 = 16 * 1024 * 1024;

pub(super) fn safe_path(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(KnowledgeError::InvalidState(
                    "Knowledge paths cannot contain symlinks".into(),
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub(crate) fn private_dir(path: &Path) -> Result<()> {
    safe_path(path)?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            private_dir(parent)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            match fs::DirBuilder::new().mode(0o700).create(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
        }
        #[cfg(not(unix))]
        fs::create_dir(path)?;
    }
    if !path.is_dir() {
        return Err(KnowledgeError::InvalidState(
            "Knowledge directory is not a directory".into(),
        ));
    }
    Ok(())
}
pub(crate) fn lock(path: &Path) -> Result<File> {
    safe_path(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(KnowledgeError::InvalidState(
            "Knowledge lock is not a regular file".into(),
        ));
    }
    fs4::FileExt::lock(&file)?;
    Ok(file)
}
pub(crate) fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    safe_path(path)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let mut file = match options.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let m = file.metadata()?;
    if !m.is_file() || m.len() > MAX_STATE_BYTES {
        return Err(KnowledgeError::InvalidState(
            "Knowledge state is not a bounded regular file".into(),
        ));
    }
    let mut data = Vec::new();
    (&mut file)
        .take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 > MAX_STATE_BYTES {
        return Err(KnowledgeError::InvalidState(
            "Knowledge state exceeds size limit".into(),
        ));
    }
    serde_json::from_slice(&data)
        .map(Some)
        .map_err(|e| KnowledgeError::InvalidState(format!("Invalid knowledge state: {e}")))
}
pub(super) fn write(path: &Path, state: &mut KnowledgeState) -> Result<()> {
    let previous = std::mem::take(&mut state.revision);
    let data =
        serde_json::to_vec(state).map_err(|e| KnowledgeError::InvalidState(e.to_string()))?;
    state.revision = planning::digest(&[&previous, &String::from_utf8_lossy(&data)]);
    let data = serde_json::to_vec_pretty(state)
        .map_err(|e| KnowledgeError::InvalidState(e.to_string()))?;
    if data.len() as u64 > MAX_STATE_BYTES {
        return Err(KnowledgeError::InvalidState(
            "Knowledge state exceeds size limit".into(),
        ));
    }
    write_bytes(path, &data)
}
/// Why: every persisted state must remain readable under the same byte limit.
/// What: reject oversized bytes before creating or replacing a file, preserving prior state.
/// Test: `checkpoint_overflow_preserves_readable_state_and_allows_recovery`.
pub(crate) fn write_bytes(path: &Path, data: &[u8]) -> Result<()> {
    // #4283: the atomic writer and bounded reader share one size contract.
    if data.len() as u64 > MAX_STATE_BYTES {
        return Err(KnowledgeError::InvalidState(
            "Knowledge state exceeds size limit".into(),
        ));
    }
    safe_path(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| KnowledgeError::InvalidState("Missing state parent".into()))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    tmp.write_all(data)?;
    tmp.as_file().sync_all()?;
    tmp.persist(path)
        .map_err(|e| KnowledgeError::Unavailable(e.to_string()))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
