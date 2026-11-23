//! The file half of an append-only, size-bounded JSONL audit log.
//!
//! Why: both brokers fail closed on a lost audit line, keep the log owner-only
//! 0600, and rotate it at 5 MiB keeping `.1` and `.2`; only the line format
//! differs between them.
//! What: [`append_line`] rotates, opens `O_APPEND|O_NOFOLLOW` 0600, refuses a
//! non-private file and writes one line in one call (fsync optional).
//! [`rotate`] shifts `log` -> `.1` -> `.2` once the live file reaches a limit.
//! Test: `append_line_creates_0600_and_refuses_a_shared_log`,
//! `rotate_keeps_two_generations`.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::private;

/// Rotate once the file reaches this size.
pub const ROTATE_BYTES: u64 = 5 * 1024 * 1024;
/// Rotated generations kept (`audit.jsonl.1` .. `.KEEP`).
pub const KEEP: usize = 2;

/// Why a line was not appended; the caller must treat it as not recorded.
#[derive(Debug, thiserror::Error)]
pub enum AppendError {
    /// The log is not an owner-only regular file.
    #[error("audit log must be an owner-only regular file")]
    NotPrivate,
    /// Rotation, open, write or fsync failed.
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// The path of rotated generation `index` of `path` (`<path>.<index>`).
pub fn generation(path: &Path, index: usize) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

/// `path` -> `.1` -> .. -> `.KEEP` once `path` is at least `limit` bytes.
///
/// A missing file is not an error; the oldest generation is replaced.
pub fn rotate(path: &Path, limit: u64) -> io::Result<()> {
    let size = match fs::symlink_metadata(path) {
        Ok(meta) => meta.len(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if size < limit {
        return Ok(());
    }
    for index in (2..=KEEP).rev() {
        let older = generation(path, index - 1);
        if older.exists() {
            fs::rename(&older, generation(path, index))?;
        }
    }
    fs::rename(path, generation(path, 1))
}

/// Append `line` (which must end in `\n`) to `path`; fail closed.
///
/// Why: a broker must not act on a call it could not log.
/// What: [`rotate`] at [`ROTATE_BYTES`], open `O_APPEND|O_NOFOLLOW` 0600,
/// refuse a non-private file, one `write_all`, then `sync_all` when `sync`.
/// Test: `append_line_creates_0600_and_refuses_a_shared_log`.
pub fn append_line(path: &Path, line: &str, sync: bool) -> Result<(), AppendError> {
    rotate(path, ROTATE_BYTES)?;
    let mut file = private::open_nofollow(path, true)?;
    if !private::private_file(&file.metadata()?) {
        return Err(AppendError::NotPrivate);
    }
    file.write_all(line.as_bytes())?;
    if sync {
        file.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn append_line_creates_0600_and_refuses_a_shared_log() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log = tmp.path().join("audit.jsonl");
        append_line(&log, "a\n", true).expect("first");
        append_line(&log, "b\n", false).expect("second");
        assert_eq!(fs::read_to_string(&log).expect("read"), "a\nb\n");
        fs::set_permissions(&log, fs::Permissions::from_mode(0o644)).expect("chmod");
        let err = append_line(&log, "c\n", true).expect_err("shared");
        assert!(matches!(err, AppendError::NotPrivate));
    }

    #[test]
    fn rotate_keeps_two_generations() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let log = tmp.path().join("audit.jsonl");
        rotate(&log, 1).expect("missing is fine");
        for body in ["one", "two", "three"] {
            fs::write(&log, body).expect("write");
            rotate(&log, 1).expect("rotate");
        }
        assert!(!log.exists());
        assert_eq!(
            fs::read_to_string(generation(&log, 1)).expect(".1"),
            "three"
        );
        assert_eq!(fs::read_to_string(generation(&log, 2)).expect(".2"), "two");
        assert!(!generation(&log, 3).exists());
        fs::write(&log, "x").expect("write");
        rotate(&log, 10).expect("below limit");
        assert!(log.exists());
    }
}
