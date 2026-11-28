//! The prototype's file primitives: `os.makedirs`, the temp-file replace and `_apply` with rollback (#12029 P2c).
//!
//! Why: these are the live-write points of `dashboard_store.py` (`os.replace`
//! and the restore after a failed write); every failure is returned, never
//! logged and passed (the fail-open check).
//! What: [`makedirs`] (the `OSError` of the deepest failing `mkdir`),
//! `atomic_write` (temp file, fsync, `rename`), [`apply`] (write in order;
//! on failure restore the earlier paths in reverse and return the restore's
//! own error when a restore fails, as Python's handler re-raises).
//! Test: `a_failed_replace_is_an_error_and_restores_the_batch`,
//! `dashboard_store_recordings_replay_clean_against_the_port` (`errors`).

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use super::{PyStoreError, dirname};
use crate::dashboard::pyjsonl::join;

/// `os.makedirs(path, exist_ok=True)`, with the `OSError` of the deepest failing `mkdir`.
pub(crate) fn makedirs(path: &str) -> Result<(), PyStoreError> {
    let trimmed = path.trim_end_matches('/');
    let head = dirname(trimmed);
    if !head.is_empty() && head != trimmed && !Path::new(head).exists() {
        match makedirs(head) {
            Err(PyStoreError::Os { errno, .. }) if errno == libc::EEXIST => {}
            other => other?,
        }
    }
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(_) if Path::new(path).is_dir() => Ok(()),
        Err(e) => Err(PyStoreError::os(path, &e)),
    }
}

/// `_atomic_write(path, payload)`: a temp file in the same directory, fsynced, then `os.replace`.
pub(super) fn atomic_write(path: &str, payload: &[u8]) -> Result<(), PyStoreError> {
    let base = path.rsplit('/').next().unwrap_or(path);
    let tmp = join(
        dirname(path),
        &format!(".{base}.{}.{}.tmp", std::process::id(), random_hex(8)),
    );
    let result = (|| {
        let mut f = File::create(&tmp).map_err(|e| PyStoreError::os(&tmp, &e))?;
        f.write_all(payload)
            .map_err(|e| PyStoreError::os(&tmp, &e))?;
        f.sync_all().map_err(|e| PyStoreError::os(&tmp, &e))?;
        // #12029: the replace's failure is the batch's failure, never a warning.
        fs::rename(&tmp, path).map_err(|e| PyStoreError::os(&tmp, &e))
    })();
    if Path::new(&tmp).exists() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `n` lowercase hex digits from the process's random hasher keys and the clock.
pub(super) fn random_hex(n: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    while out.len() < n {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out.truncate(n);
    out
}

/// `dj.fsync_dir(path)`.
fn fsync_dir(path: &str) -> Result<(), PyStoreError> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|e| PyStoreError::os(path, &e))
}

/// `Store._apply(plan, fsync_dirs)`: write in order; on failure restore every earlier path.
pub(super) fn apply(
    plan: &[(String, Option<Vec<u8>>)],
    fsync_dirs: bool,
) -> Result<(), PyStoreError> {
    let mut backups: Vec<(&str, Option<Vec<u8>>)> = Vec::new();
    fn step<'a>(
        path: &'a str,
        payload: &Option<Vec<u8>>,
        backups: &mut Vec<(&'a str, Option<Vec<u8>>)>,
        fsync_dirs: bool,
    ) -> Result<(), PyStoreError> {
        let old = if Path::new(path).exists() {
            Some(fs::read(path).map_err(|e| PyStoreError::os(path, &e))?)
        } else {
            None
        };
        let had = old.is_some();
        backups.push((path, old));
        match payload {
            None if had => fs::remove_file(path).map_err(|e| PyStoreError::os(path, &e))?,
            None => {}
            Some(bytes) => {
                makedirs(dirname(path))?;
                atomic_write(path, bytes)?;
            }
        }
        if fsync_dirs {
            fsync_dir(dirname(path))?;
        }
        Ok(())
    }
    for (path, payload) in plan {
        if let Err(first) = step(path, payload, &mut backups, fsync_dirs) {
            for (path, old) in backups.iter().rev() {
                match old {
                    None if Path::new(path).exists() => {
                        fs::remove_file(path).map_err(|e| PyStoreError::os(path, &e))?;
                    }
                    None => {}
                    Some(bytes) => atomic_write(path, bytes)?,
                }
            }
            return Err(first);
        }
    }
    Ok(())
}
