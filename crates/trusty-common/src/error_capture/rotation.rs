//! Size-capped rotation for the `errors.jsonl` error store (#8028).
//!
//! Why: the store appended forever. An 88 MB copy of one daemon's file was
//! seen, and the reader loaded the whole file into memory on every start.
//! What: [`rotate_if_due`] renames the live file to `<file>.1` once it reaches
//! [`RotationPolicy::max_bytes`], shifting older slots up and keeping at most
//! [`RotationPolicy::keep`] of them. [`read_tail`] bounds how much of one file a
//! reader loads. Rotation runs under the cross-process [`crate::file_lock`]
//! sidecar lock, because the daemon and every `tm` CLI process append to the
//! same file.
//! Test: `store_tests::writing_past_the_cap_rotates_and_bounds_disk_use`,
//! `store_tests::oversized_legacy_file_is_compacted_on_rotation`.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Live-file size that triggers a rotation: 4 MiB, about 10k typical records.
pub const DEFAULT_ROTATE_AT_BYTES: u64 = 4 * 1024 * 1024;

/// Rotated files kept beside the live one (`errors.jsonl.1`, `errors.jsonl.2`).
pub const DEFAULT_ROTATED_KEEP: usize = 2;

/// Wait bound for the rotation lock. Rotation is a rename, so a holder
/// releases it within milliseconds; the bound only stops a wedged holder from
/// stalling the tracing hot path indefinitely.
const ROTATION_LOCK_TIMEOUT: Duration = Duration::from_secs(2);

/// Size bound and retention for one store file.
///
/// Why/What: the live file rotates once it reaches `max_bytes`; `keep` rotated
/// files survive (clamped to at least one). On-disk use per store is therefore
/// at most `(keep + 1) * (max_bytes + one record)`.
/// Test: `store_tests::writing_past_the_cap_rotates_and_bounds_disk_use`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RotationPolicy {
    /// Live-file size, in bytes, at which the next append rotates first.
    pub max_bytes: u64,
    /// Number of rotated files kept; the oldest beyond this is overwritten.
    pub keep: usize,
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_ROTATE_AT_BYTES,
            keep: DEFAULT_ROTATED_KEEP,
        }
    }
}

impl RotationPolicy {
    fn keep(self) -> usize {
        self.keep.max(1)
    }

    /// How much of one file a reader loads: twice the cap holds any file this
    /// policy wrote whole, and bounds a legacy file written before #8028.
    pub(crate) fn read_limit(self) -> u64 {
        self.max_bytes.max(1).saturating_mul(2)
    }
}

/// The `n`th rotated sibling of `path`: `errors.jsonl` -> `errors.jsonl.<n>`.
pub(crate) fn rotated_path(path: &Path, n: usize) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(".{n}"));
    PathBuf::from(name)
}

/// Every file of one store, newest first: the live file, then `.1`, `.2`, ...
pub(crate) fn files_newest_first(path: &Path, policy: RotationPolicy) -> Vec<PathBuf> {
    let mut files = vec![path.to_path_buf()];
    files.extend((1..=policy.keep()).map(|n| rotated_path(path, n)));
    files
}

/// Rotate `path` when it has reached the policy cap; a no-op below it.
///
/// Why: see the module doc. What: takes the sidecar lock, re-checks the size
/// (another process may have rotated first), then shifts `.k-1` over `.k` down
/// to `.1` and moves the live file into `.1`. Every step is a `rename(2)`, so a
/// reader or writer sees the old file or the new one, never a partial one.
/// Returns `Err` when the lock or any step fails; the caller decides what that
/// means for the pending append.
/// Test: `store_tests::a_rotation_failure_refuses_the_disk_write_and_counts_it`.
pub(crate) fn rotate_if_due(path: &Path, policy: RotationPolicy) -> std::io::Result<()> {
    if !is_due(path, policy) {
        return Ok(());
    }
    crate::file_lock::with_exclusive_lock_timeout(path, ROTATION_LOCK_TIMEOUT, || {
        if !is_due(path, policy) {
            return Ok(());
        }
        for n in (1..policy.keep()).rev() {
            let from = rotated_path(path, n);
            if from.exists() {
                std::fs::rename(&from, rotated_path(path, n + 1))?;
            }
        }
        move_live_into_first_slot(path, policy)
    })?
}

fn is_due(path: &Path, policy: RotationPolicy) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() >= policy.max_bytes)
}

/// Move the live file into `.1`. A normal file is renamed. A legacy file over
/// the read limit keeps only its tail: written to a temp file, then renamed
/// over `.1`, so the slot never holds a partial write.
fn move_live_into_first_slot(path: &Path, policy: RotationPolicy) -> std::io::Result<()> {
    let first = rotated_path(path, 1);
    let len = std::fs::metadata(path)?.len();
    if len <= policy.read_limit() {
        return std::fs::rename(path, first);
    }
    // #8028: an unbounded pre-fix file would otherwise sit in a rotated slot
    // at full size until it aged out.
    let tail = read_tail(path, policy.max_bytes)?;
    let mut tmp = first.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, tail)?;
    std::fs::rename(&tmp, &first)?;
    std::fs::remove_file(path)
}

/// Read at most the last `limit` bytes of `path`, starting at a line boundary.
///
/// Why: a reader must never load an unbounded file. What: when the file is
/// longer than `limit`, seeks to `len - limit` and drops the partial first
/// line; otherwise returns the whole file.
/// Test: `store_tests::oversized_legacy_file_is_compacted_on_rotation`.
pub(crate) fn read_tail(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut buf = Vec::new();
    if len <= limit {
        file.read_to_end(&mut buf)?;
        return Ok(buf);
    }
    file.seek(SeekFrom::Start(len - limit))?;
    file.read_to_end(&mut buf)?;
    let start = buf
        .iter()
        .position(|b| *b == b'\n')
        .map_or(buf.len(), |i| i + 1);
    Ok(buf.split_off(start))
}
