//! The cross-process JSON-file mechanism `sessions.json` and its siblings share.
//!
//! Why: the daemon and `tm supervisor` are separate processes reading and
//! writing the same files in `<data_dir>` (#1219). Two rules make that safe, and
//! both have already been learned the hard way here: a reader must re-stat the
//! file before trusting its in-memory copy, and a writer must stage through a
//! private temp file and rename, because a plain truncate-and-rewrite lets a
//! concurrent reader observe a half-written file. `SessionStore` grew both;
//! `resume_breaker.rs` (#6568) needs exactly the same two, over a file in the
//! same directory. A second, weaker copy is the defect this module prevents —
//! see CLAUDE.md's common-entry-point rule.
//!
//! What: [`staging_path`] mints the per-instance temp name and [`write_atomic`]
//! does the stage-and-rename — both callers use both. [`FileSig`], [`sig_of`]
//! and [`is_unchanged`] are the freshness fingerprint, which `SessionStore`
//! uses and the breaker sidecar deliberately does not: that payload keeps a
//! constant serialized length across a cycle, so the fingerprint would rest on
//! mtime alone and could skip a needed reload on a coarse-mtime filesystem (see
//! `resume_breaker::ResumeBreakerStore`). Nothing here parses or knows about any
//! payload type — the caller owns serialisation, so the same primitives serve a
//! `Vec<SessionRecord>` and a `HashMap<String, FlapState>` alike.
//!
//! What this is NOT: a lock. Two processes that read-modify-write concurrently
//! can still lose one update — the rename makes each write ATOMIC, not
//! EXCLUSIVE. Both current callers tolerate that (a lost session-state write is
//! re-derived on the next reconcile; a lost flap counter delays a park by one
//! cycle), and neither may be extended to data where a lost update is
//! unacceptable without adding real locking first.
//!
//! Test: `json_file_tests.rs`; the cross-process reload path is additionally
//! covered by `store_reload_picks_up_external_write` and, for the #6568
//! sidecar, `two_managers_over_one_data_dir_still_park_a_flapping_session`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use tokio::fs;

/// A cheap freshness fingerprint for a backing file.
///
/// Why: the cross-process reload check (#1219) keys off "did the file change
/// since we last touched it". An mtime alone is insufficient on filesystems
/// whose mtime resolution is coarse (1s on some): two writes in the same second
/// would compare equal and the reader would miss the second write. Pairing the
/// mtime with the byte length catches a same-second write that changed the file
/// size, which a state transition (different JSON length) almost always does.
/// What: the file's last-modified `SystemTime` (an `Option` — `None` when the
/// platform/filesystem cannot report an mtime) and its length in bytes. The
/// whole `FileSig` is wrapped in an `Option` by callers, where `None` means
/// "file absent / could not be stat'd".
/// Test: `store_reload_picks_up_external_write`, `store_reload_noop_when_unchanged`,
/// `sig_of_is_none_for_a_missing_file`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct FileSig {
    /// File modification time, if the platform/filesystem reports one.
    pub(crate) mtime: Option<SystemTime>,
    /// File length in bytes.
    pub(crate) len: u64,
}

/// True when `current` and `last` prove the file has not changed since `last`.
///
/// Why: every caller made the same three-case judgement by hand — present and
/// equal is unchanged, and a `None` on EITHER side (absent now, or never
/// observed) must read as CHANGED so an external write is never missed. Stating
/// it once keeps a future caller from inverting the `None` case, which would
/// serve stale data silently.
/// What: `true` only when both are `Some` and equal.
/// Test: `unchanged_requires_both_signatures`.
pub(crate) fn is_unchanged(current: Option<FileSig>, last: Option<FileSig>) -> bool {
    matches!((current, last), (Some(a), Some(b)) if a == b)
}

/// Stat `path` and return its freshness fingerprint, or `None` if absent.
///
/// Why: the reload check and the save path need the same (mtime, len) signature;
/// centralising it keeps "what counts as this file's identity" in one place.
/// What: on `metadata` success returns `Some(FileSig)`; on any stat error (most
/// commonly: the file does not exist) returns `None`, which
/// [`is_unchanged`] reads as changed.
/// Test: `sig_of_is_none_for_a_missing_file`, `sig_of_changes_after_a_write`.
pub(crate) async fn sig_of(path: &Path) -> Option<FileSig> {
    let meta = fs::metadata(path).await.ok()?;
    Some(FileSig {
        // `modified()` can be unsupported on exotic filesystems; `None` there
        // makes the (mtime, len) pair compare unequal so reads reload — correct,
        // just less efficient — rather than risk serving stale data.
        mtime: meta.modified().ok(),
        len: meta.len(),
    })
}

/// The private staging path one writer instance renames over `path`.
///
/// Why (#5007): saving used to stage through `path.with_extension("json.tmp")` —
/// one fixed name shared by every writer of that file, in every process. The
/// rename is atomic, but the staging WRITE is not exclusive: two writers racing
/// on one staging name interleave their bytes and the survivor renames a
/// corrupt file into place.
/// Two writers racing on one staging name interleave their bytes, and the
/// survivor renames a document with the length of the longer serialization and
/// the head of the shorter one into place — the exact shape #5007 reported.
/// What: `<path>.tmp.<pid>.<uuid>` — the pid separates processes, the uuid
/// separates instances within one process (tests routinely hold two stores over
/// one directory, and `agent_reset_workspace` builds its own alongside the
/// daemon's).
/// Test: `two_stores_over_one_path_do_not_share_a_staging_file`,
/// `staging_paths_are_unique_per_instance`.
pub(crate) fn staging_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    path.with_file_name(name)
}

/// Write `bytes` to `path` atomically, staging through `tmp`.
///
/// Why: a plain `fs::write` truncates then rewrites, so a reader in the other
/// process can observe an empty or half-written file and parse it as corrupt.
/// A rename over the target is atomic on POSIX, so a cross-process reader always
/// sees either the old file or the new one, whole.
/// What: creates `path`'s parent if needed, writes `tmp`, renames it over
/// `path`. On a failed rename the staging file is removed — it is named after
/// this process and nothing else would ever clean it up — and the error is
/// returned.
/// Test: `write_atomic_replaces_the_target`,
/// `write_atomic_leaves_no_staging_file_behind`.
pub(crate) async fn write_atomic(path: &Path, tmp: &Path, bytes: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(tmp, bytes).await?;
    if let Err(e) = fs::rename(tmp, path).await {
        let _ = fs::remove_file(tmp).await;
        return Err(e);
    }
    Ok(())
}
