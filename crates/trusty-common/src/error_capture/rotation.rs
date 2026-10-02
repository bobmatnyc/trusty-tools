//! Size-capped rotation for the `errors.jsonl` error store (#8028).
//!
//! Why: the store appended forever. An 88 MB copy of one daemon's file was
//! seen, and the reader loaded the whole file into memory on every start.
//! What: [`rotate_if_due`] renames the live file to `<file>.1` once it reaches
//! [`RotationPolicy::max_bytes`], shifting older slots up and keeping at most
//! [`RotationPolicy::keep`] of them. [`read_tail_from`] bounds how much of one
//! file a reader loads. Rotation runs under the cross-process
//! [`crate::file_lock`] sidecar lock, because the daemon and every `tm` CLI
//! process append to the same file. Appends take no lock, so every step must
//! keep a record written by a concurrent appender.
//! Test: `store_tests::writing_past_the_cap_rotates_and_bounds_disk_use`,
//! `store_tests::oversized_legacy_file_is_compacted_on_rotation`,
//! `compaction_tests::compaction_keeps_records_appended_during_it`.

use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Live-file size that triggers a rotation: 4 MiB, about 10k typical records.
pub const DEFAULT_ROTATE_AT_BYTES: u64 = 4 * 1024 * 1024;

/// Rotated files kept beside the live one (`errors.jsonl.1`, `errors.jsonl.2`).
pub const DEFAULT_ROTATED_KEEP: usize = 2;

/// Wait bound for the rotation lock. Rotation is a rename, so a holder
/// releases it within milliseconds. #8028: this runs on the tracing hot path,
/// so a wedged holder costs each ERROR at most this long; a refused write is
/// counted and the next append retries the rotation.
const ROTATION_LOCK_TIMEOUT: Duration = Duration::from_millis(50);

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

/// `path` with `suffix` appended to its file name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Move the live file into `.1`. A normal file is renamed. A legacy file over
/// the read limit keeps only its tail (see [`compact_legacy_into_first_slot`]).
fn move_live_into_first_slot(path: &Path, policy: RotationPolicy) -> std::io::Result<()> {
    let first = rotated_path(path, 1);
    let len = std::fs::metadata(path)?.len();
    let src = with_suffix(&first, ".src");
    // #8028: a leftover source from a failed compaction is never overwritten;
    // this rotation keeps the whole live file instead.
    if len <= policy.read_limit() || src.is_file() {
        return std::fs::rename(path, first);
    }
    compact_legacy_into_first_slot(path, &first, &src, policy)
}

/// Compact an oversized legacy live file into `.1`, losing no record.
///
/// Why: an unbounded pre-fix file would otherwise sit in a rotated slot at
/// full size until it aged out, and appends take no lock, so compacting the
/// live file in place deleted any record appended mid-compaction (#8028).
/// What: renames the live file to `.1.src` first, so every later append opens
/// a fresh live file. Writes the source's tail to `.1.tmp`, then copies any
/// bytes a writer holding the old descriptor added since the read, then
/// renames the temp file over `.1` and removes the source. Fail-open: if the
/// first rename fails, the live file is untouched; if the compaction fails,
/// the source is renamed whole into `.1`; if that fails too, the source stays
/// on disk. No branch deletes a record, and every branch returns promptly.
/// One window remains: a writer that opened the live file before the rename
/// and writes only after the copy finished writes into the removed source.
/// Appends open the file per record, so that is one in-flight write.
/// Test: `compaction_tests::compaction_keeps_records_appended_during_it`,
/// `compaction_tests::a_failed_source_rename_leaves_the_live_file_intact`,
/// `compaction_tests::a_failed_compaction_keeps_the_whole_source_in_the_first_slot`,
/// `compaction_tests::a_failed_fallback_keeps_the_source_and_the_writer_moving`.
fn compact_legacy_into_first_slot(
    path: &Path,
    first: &Path,
    src: &Path,
    policy: RotationPolicy,
) -> std::io::Result<()> {
    std::fs::rename(path, src)?;
    let tmp = with_suffix(first, ".tmp");
    let compacted = write_compacted(src, &tmp, policy.max_bytes).and_then(|()| {
        super::test_hook::fire(super::test_hook::Point::CompactionCopied);
        std::fs::rename(&tmp, first)
    });
    if let Err(e) = compacted {
        eprintln!(
            "[bug-capture] compacting {} failed ({e}); keeping it whole",
            src.display()
        );
        let _ = std::fs::remove_file(&tmp);
        return std::fs::rename(src, first);
    }
    if let Err(e) = std::fs::remove_file(src) {
        // The tail is already in `.1`; a stale source only costs disk space.
        eprintln!("[bug-capture] removing {}: {e}", src.display());
    }
    Ok(())
}

/// Write the last `limit` bytes of `src` to `tmp`, plus anything appended to
/// `src` after that read by a writer that opened it before the rename.
fn write_compacted(src: &Path, tmp: &Path, limit: u64) -> std::io::Result<()> {
    let mut source = std::fs::File::open(src)?;
    let (tail, read_to) = read_tail_from(&mut source, limit)?;
    super::test_hook::fire(super::test_hook::Point::CompactionTailRead);
    let mut out = std::fs::File::create(tmp)?;
    std::io::Write::write_all(&mut out, &tail)?;
    // #8028: an O_APPEND write through a descriptor opened before the rename
    // lands in the source, after the tail we read.
    source.seek(SeekFrom::Start(read_to))?;
    std::io::copy(&mut source, &mut out)?;
    out.sync_all()
}

/// Read at most the last `limit` bytes of an open file, from a line boundary.
///
/// Why: a reader must never load an unbounded file, and the compaction and
/// the reader both work on a handle they opened (#8028).
/// What: when the file is longer than `limit`, seeks to `len - limit` and
/// drops the partial first line; otherwise returns the whole file. Also
/// returns the offset the read stopped at.
/// Test: `store_tests::oversized_legacy_file_is_compacted_on_rotation`.
pub(crate) fn read_tail_from(
    file: &mut std::fs::File,
    limit: u64,
) -> std::io::Result<(Vec<u8>, u64)> {
    let len = file.metadata()?.len();
    let start_at = len.saturating_sub(limit);
    file.seek(SeekFrom::Start(start_at))?;
    let mut buf = Vec::new();
    let read = file.read_to_end(&mut buf)? as u64;
    let end = start_at + read;
    if start_at == 0 {
        return Ok((buf, end));
    }
    let start = buf
        .iter()
        .position(|b| *b == b'\n')
        .map_or(buf.len(), |i| i + 1);
    Ok((buf.split_off(start), end))
}
