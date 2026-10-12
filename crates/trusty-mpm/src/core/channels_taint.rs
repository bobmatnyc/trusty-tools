//! The channel taint contract: one writer, reader and retirer of the taint
//! files a Slack-originated answer leaves on a session (#8433 P0).
//!
//! Why: the daemon taints a Claude session before it delivers an inbound
//! channel answer to it, and the egress hook reads that taint to refuse the
//! session's path tool calls. Both sides must agree on the path, the id
//! sanitiser and what "tainted" means, so this one module serves both (owner
//! ruling Dfb); S4 imports [`sanitize_id`], [`taint_path`] and [`read_state`].
//! What: one file per Claude `session_id` at
//! `~/.trusty-tools/trusty-mpm/channels/taint/<session_id>.json`, written
//! atomically with mode 0600 ([`write_taint`]). The managed session's own
//! taint lives beside them at `managed/<managed_id>.json` ([`mark_managed`]),
//! inside the same trust anchor, so it survives a Claude `session_id` change
//! (Dfc) and [`reapply`] re-writes the taint for the new id. Only [`retire`]
//! removes either, when the managed session ends; no timer exists. The
//! verdict reads presence only: absent is untainted, and every other outcome,
//! including an unreadable or malformed file and a bad id, is tainted.
//! Test: `channels_taint_tests.rs`.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The record format version.
pub const FORMAT_VERSION: u32 = 1;

/// The `channel` a hook-side re-apply writes when the daemon did not (D09).
pub const REAPPLY_FAILED_CHANNEL: &str = "reapply-failed";

/// The subdirectory holding the per-managed-session markers.
const MANAGED_DIR: &str = "managed";

/// The file the hook appends to when its own re-apply write fails too (D09).
pub const REAPPLY_FAILURES_FILE: &str = "reapply-failures.jsonl";

/// The longest id accepted as a file name.
const MAX_ID_LEN: usize = 128;

/// A taint operation that could not be completed.
#[derive(Debug, thiserror::Error)]
pub enum TaintError {
    /// The id is not a plain session id, so no path is built from it.
    #[error("`{0}` is not a plain session id (ASCII letters, digits, `-`, `_`; 1-128 chars)")]
    BadId(String),
    /// A filesystem step failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The path the step acted on.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// A marker or taint file exists but is not a taint record.
    #[error("{}: not a taint record: {reason}", path.display())]
    Malformed {
        /// The file.
        path: PathBuf,
        /// What the parser said.
        reason: String,
    },
}

/// One taint file's content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaintRecord {
    /// [`FORMAT_VERSION`].
    pub v: u32,
    /// The Claude session id the file is named for.
    pub session_id: String,
    /// The managed session id, or empty when the writer could not know it.
    pub managed_session: String,
    /// The channel the answer came from, or [`REAPPLY_FAILED_CHANNEL`].
    pub channel: String,
    /// The route that admitted the answer.
    pub route: String,
    /// The question the answer resolved.
    pub question_id: String,
    /// When the taint was first set.
    pub tainted_at: DateTime<Utc>,
}

/// What a managed session was tainted by; the record flag (Dfc).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaintMark {
    /// The channel the answer came from.
    pub channel: String,
    /// The route that admitted it.
    pub route: String,
    /// The question it resolved.
    pub question_id: String,
    /// When the managed session was first tainted.
    pub tainted_at: DateTime<Utc>,
}

impl TaintMark {
    /// The taint record this mark writes for one Claude session id.
    pub fn record_for(&self, session_id: &str, managed_session: &str) -> TaintRecord {
        TaintRecord {
            v: FORMAT_VERSION,
            session_id: session_id.to_owned(),
            managed_session: managed_session.to_owned(),
            channel: self.channel.clone(),
            route: self.route.clone(),
            question_id: self.question_id.clone(),
            tainted_at: self.tainted_at,
        }
    }
}

/// The taint directory under `home`.
pub fn taint_dir(home: &Path) -> PathBuf {
    trusty_common::crate_config::crate_config_dir_at(
        home,
        crate::core::trusty_tools_config::CRATE_NAME,
    )
    .join("channels")
    .join("taint")
}

/// `id` when it is safe as a file name, else [`TaintError::BadId`].
///
/// Why: the id comes from a hook payload, so a `/`, `..` or NUL must never
/// build a path outside the taint directory.
/// What: 1-128 chars of ASCII letters, digits, `-` and `_`; nothing else.
/// Test: `session_id_with_slash_or_dotdot_is_refused`.
pub fn sanitize_id(id: &str) -> Result<&str, TaintError> {
    let ok = !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(id)
    } else {
        Err(TaintError::BadId(id.to_owned()))
    }
}

/// The taint file for `session_id` in `dir`; a bad id is an `Err`.
pub fn taint_path(dir: &Path, session_id: &str) -> Result<PathBuf, TaintError> {
    Ok(dir.join(format!("{}.json", sanitize_id(session_id)?)))
}

/// The managed-session marker for `managed` in `dir`.
fn managed_path(dir: &Path, managed: &str) -> Result<PathBuf, TaintError> {
    Ok(dir
        .join(MANAGED_DIR)
        .join(format!("{}.json", sanitize_id(managed)?)))
}

/// Write `bytes` to `path` atomically with mode 0600.
///
/// What: creates the directory (0700), writes a unique temp file beside the
/// target, syncs it and renames it over the target. Any step's error is
/// returned, and the temp file is removed on failure.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), TaintError> {
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
    let io = |path: &Path, source| TaintError::Io {
        path: path.to_owned(),
        source,
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| io(dir, e))?;
    write_fault().map_err(|e| io(path, e))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(source) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(TaintError::Io {
            path: path.to_owned(),
            source,
        });
    }
    Ok(())
}

/// Write `record` to its taint file in `dir`; the path written.
///
/// Why: a caller that cannot taint must not deliver, so every failure is an
/// `Err` the caller acts on, never a logged warning.
/// What: [`taint_path`] for `record.session_id`, then an atomic 0600 write.
/// Test: `taint_write_is_atomic_0600_and_names_session`,
/// `taint_write_failure_returns_err`.
pub fn write_taint(dir: &Path, record: &TaintRecord) -> Result<PathBuf, TaintError> {
    let path = taint_path(dir, &record.session_id)?;
    let bytes = serde_json::to_vec(record).map_err(|e| TaintError::Malformed {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    write_atomic(&path, &bytes)?;
    Ok(path)
}

/// Why a session reads tainted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaintReason {
    /// A well-formed taint file is present.
    Present,
    /// A file is present but cannot be read.
    Unreadable,
    /// A file is present but is not a taint record.
    Malformed,
    /// The id is not a plain session id.
    BadId,
}

/// The verdict for one Claude session id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaintState {
    /// No taint file: the one allow-on-absence.
    Untainted,
    /// Tainted, and why.
    Tainted(TaintReason),
}

impl TaintState {
    /// Whether the session must be treated as tainted.
    pub fn is_tainted(self) -> bool {
        matches!(self, Self::Tainted(_))
    }
}

/// Whether `session_id` is tainted, reading `dir`.
///
/// Why: fail closed. Only a file that is provably absent reads untainted.
/// What: a bad id, any read error other than `NotFound`, and content that is
/// not a [`TaintRecord`] each read tainted. Age is never consulted (Dfc).
/// Test: `absent_taint_reads_untainted`, `unreadable_taint_reads_as_tainted`,
/// `malformed_taint_reads_as_tainted`, `no_timer_retires_taint`.
pub fn read_state(dir: &Path, session_id: &str) -> TaintState {
    let Ok(path) = taint_path(dir, session_id) else {
        return TaintState::Tainted(TaintReason::BadId);
    };
    match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => TaintState::Untainted,
        Err(_) => TaintState::Tainted(TaintReason::Unreadable),
        Ok(bytes) if serde_json::from_slice::<TaintRecord>(&bytes).is_ok() => {
            TaintState::Tainted(TaintReason::Present)
        }
        Ok(_) => TaintState::Tainted(TaintReason::Malformed),
    }
}

/// Record that managed session `managed` is tainted by `mark` (Dfc).
///
/// Test: `stop_and_decommission_retire_taint`.
pub fn mark_managed(dir: &Path, managed: &str, mark: &TaintMark) -> Result<(), TaintError> {
    let path = managed_path(dir, managed)?;
    let bytes = serde_json::to_vec(mark).map_err(|e| TaintError::Malformed {
        path: path.clone(),
        reason: e.to_string(),
    })?;
    write_atomic(&path, &bytes)
}

/// The mark on managed session `managed`: `Ok(None)` only when provably absent.
///
/// What: an unreadable or malformed marker is an `Err`, never `None`.
pub fn managed_mark(dir: &Path, managed: &str) -> Result<Option<TaintMark>, TaintError> {
    let path = managed_path(dir, managed)?;
    let bytes = match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(TaintError::Io { path, source }),
        Ok(bytes) => bytes,
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| TaintError::Malformed {
            path,
            reason: e.to_string(),
        })
}

/// Taint managed session `managed` and its current Claude id (P5's writer).
///
/// What: the Claude id's file first, then the managed marker, so a failure
/// at either step is an `Err` the caller turns into "do not deliver".
/// Test: `session_start_with_new_id_reapplies_taint_for_tainted_managed_session`.
pub fn taint_managed_session(
    dir: &Path,
    managed: &str,
    claude_session_id: &str,
    mark: &TaintMark,
) -> Result<(), TaintError> {
    write_taint(dir, &mark.record_for(claude_session_id, managed))?;
    mark_managed(dir, managed, mark)
}

/// Re-apply `managed`'s taint to a new Claude id; `Ok(true)` when written.
///
/// Why (Dfc, D09): a new `session_id` in a tainted managed session must read
/// tainted before its first tool call, and a failure must be visible to the
/// SessionStart caller, never logged and dropped.
/// What: `Ok(false)` when the managed session carries no mark; otherwise
/// writes the taint file for `claude_session_id`. A marker that cannot be
/// read and a write that fails are each an `Err`.
/// Test: `session_start_with_new_id_reapplies_taint_for_tainted_managed_session`,
/// `session_start_for_untainted_managed_session_writes_nothing`.
pub fn reapply(dir: &Path, managed: &str, claude_session_id: &str) -> Result<bool, TaintError> {
    let Some(mark) = managed_mark(dir, managed)? else {
        return Ok(false);
    };
    write_taint(dir, &mark.record_for(claude_session_id, managed))?;
    Ok(true)
}

/// Whether any taint file or managed marker exists under `dir`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// The directory is absent or holds no file.
    None,
    /// At least one file or marker is present.
    Some,
    /// The directory or an entry could not be read.
    Unknown,
}

/// [`Presence`] of taint under `dir`, the markers in `managed/` included.
///
/// Test: `reapply_daemon_unreachable_at_session_start_egress_denies`,
/// `daemon_unreachable_with_no_taint_anywhere_writes_nothing`.
pub fn presence(dir: &Path) -> Presence {
    let mut seen = Presence::None;
    for sub in [dir.to_path_buf(), dir.join(MANAGED_DIR)] {
        let entries = match std::fs::read_dir(&sub) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Presence::Unknown,
            Ok(entries) => entries,
        };
        for entry in entries {
            let Ok(entry) = entry else {
                return Presence::Unknown;
            };
            if entry.file_name() != MANAGED_DIR {
                seen = Presence::Some;
            }
        }
    }
    seen
}

/// One listed taint file: its path, and its record or why it is unreadable.
pub type ListedTaint = (PathBuf, Result<TaintRecord, String>);

/// Every taint file in `dir` with its parse, for the doctor row.
///
/// What: `*.json` entries of `dir` (not `managed/`), each with its record or
/// the reason it could not be read; temp files are skipped.
pub fn list_taints(dir: &Path) -> Result<Vec<ListedTaint>, TaintError> {
    let io = |source| TaintError::Io {
        path: dir.to_owned(),
        source,
    };
    let entries = match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        other => other.map_err(io)?,
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry.map_err(io)?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let parsed = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()));
        out.push((path, parsed));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// The managed ids that carry a marker in `dir`.
pub fn marked_managed(dir: &Path) -> Result<Vec<String>, TaintError> {
    let sub = dir.join(MANAGED_DIR);
    let entries = match std::fs::read_dir(&sub) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        other => other.map_err(|source| TaintError::Io {
            path: sub.clone(),
            source,
        })?,
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| TaintError::Io {
            path: sub.clone(),
            source,
        })?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json")
            && let Some(stem) = path.file_stem()
        {
            out.push(stem.to_string_lossy().into_owned());
        }
    }
    out.sort();
    Ok(out)
}

/// Retire managed session `managed`: remove its taint files, then its mark.
///
/// Why (Dfc): a taint ends only when the managed session ends. A partial
/// retire must leave the session tainted, so the mark goes last and only
/// when every file is gone.
/// What: removes each `*.json` in `dir` whose `managed_session` is
/// `managed`. A file that cannot be read or parsed is left in place and
/// makes the retire an `Err`, as does a failed removal; the mark is then
/// kept. Returns how many taint files were removed.
/// Test: `retire_removes_only_that_managed_sessions_files`,
/// `stop_and_decommission_retire_taint`.
pub fn retire(dir: &Path, managed: &str) -> Result<usize, TaintError> {
    let marker = managed_path(dir, managed)?;
    let mut removed = 0;
    let mut first_err = None;
    for (path, parsed) in list_taints(dir)? {
        match parsed {
            Ok(record) if record.managed_session != managed => {}
            Ok(_) => match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(source) => {
                    first_err.get_or_insert(TaintError::Io { path, source });
                }
            },
            Err(reason) => {
                first_err.get_or_insert(TaintError::Malformed { path, reason });
            }
        }
    }
    if let Some(err) = first_err {
        return Err(err);
    }
    match std::fs::remove_file(&marker) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(TaintError::Io {
            path: marker,
            source: e,
        }),
        _ => Ok(removed),
    }
}

/// Test seam: make every [`write_taint`] / [`mark_managed`] in THIS process
/// fail, to force the daemon-side write to fail in an end-to-end test (D09).
///
/// Not gated on `cfg(test)` because integration tests compile the library
/// without it; production never calls it.
#[doc(hidden)]
pub fn inject_write_failure_for_test(on: bool) {
    WRITE_FAULT.store(on, std::sync::atomic::Ordering::SeqCst);
}

/// The fault [`inject_write_failure_for_test`] arms.
static WRITE_FAULT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// `Err` while the test fault is armed.
fn write_fault() -> std::io::Result<()> {
    if WRITE_FAULT.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(std::io::Error::other(
            "injected taint write failure (test seam)",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "channels_taint_tests.rs"]
mod tests;
