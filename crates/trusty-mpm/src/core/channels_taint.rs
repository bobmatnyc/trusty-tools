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
//! (Dfc) and [`reapply`] re-writes the taint for the new id. The mark names
//! the session's workspace, so [`covering_mark`] scopes the SessionStart
//! hook's own re-apply (D09) to that workspace. Each taint record carries its
//! own workspace scope, which a retire's hand-off keeps (C1). Only [`retire`]
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

/// The file in the taint directory the hook appends to when its own re-apply
/// write fails too (D09). It sits under the trust anchor so no session can
/// delete it, and its `.jsonl` name keeps it out of [`list_taints`].
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
    /// A taint record names no workspace scope, so a retire cannot tell which
    /// other marks still cover it (#8433 C1).
    #[error(
        "{}: the taint record has no workspace scope, so a retire keeps it",
        path.display()
    )]
    Unscoped {
        /// The file.
        path: PathBuf,
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
    /// The directory the tainted id's cwd is at or under; a relative or empty
    /// path is every cwd. It survives a retire's hand-off, so the heir is
    /// chosen by this scope (#8433 C1). `None` only in a record written
    /// before the field existed, which no retire removes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
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
    /// The managed session's working directory, absolute. A `SessionStart`
    /// the daemon does not confirm taints the new id only when its cwd is at
    /// or under this path (D09); a relative or empty path covers every cwd.
    pub workspace: PathBuf,
}

/// Which managed session's taint covers a `SessionStart` at one cwd (D09).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cover {
    /// No mark covers the cwd: there is nothing to re-apply.
    None,
    /// This managed session's mark covers the cwd, or cannot be read.
    Managed(String),
    /// The marks cannot be listed, so any session could be covered.
    Unknown,
}

impl TaintMark {
    /// Whether this mark covers a `SessionStart` at the canonical `cwd`.
    ///
    /// What: an unknown cwd and a relative or empty workspace cover every
    /// cwd (fail closed); otherwise `cwd` must be the workspace or under it,
    /// compared after canonicalizing the workspace.
    pub fn covers(&self, cwd: Option<&Path>) -> bool {
        cwd.is_none_or(|cwd| scope_covers(&self.workspace, cwd))
    }

    /// The taint record this mark writes for one Claude session id, scoped
    /// to this mark's workspace.
    pub fn record_for(&self, session_id: &str, managed_session: &str) -> TaintRecord {
        TaintRecord {
            v: FORMAT_VERSION,
            session_id: session_id.to_owned(),
            managed_session: managed_session.to_owned(),
            channel: self.channel.clone(),
            route: self.route.clone(),
            question_id: self.question_id.clone(),
            tainted_at: self.tainted_at,
            workspace: Some(self.workspace.clone()),
        }
    }
}

/// `path` canonicalized, or as given when it cannot be.
fn canon(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Whether the canonical `cwd` is at or under `scope`; a relative or empty
/// scope covers every cwd (fail closed).
fn scope_covers(scope: &Path, cwd: &Path) -> bool {
    !scope.is_absolute() || cwd.starts_with(canon(scope))
}

/// Whether some cwd is covered by both scopes: either is relative or empty,
/// or one is at or under the other (#8433 B1, C1).
fn scopes_overlap(a: &Path, b: &Path) -> bool {
    scope_covers(a, &canon(b)) || scope_covers(b, &canon(a))
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
    mark_managed(dir, managed, mark)?;
    handshake_gap();
    Ok(())
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
    handshake_gap();
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

/// The entry names of `dir`, sorted; an absent directory has none.
///
/// What: one listing for [`presence`], [`list_taints`], [`marked_managed`]
/// and [`covering_mark`], so a directory or entry read error is one `Err`
/// each caller fails closed on.
fn entry_names(dir: &Path) -> Result<Vec<String>, TaintError> {
    let io = |source| TaintError::Io {
        path: dir.to_owned(),
        source,
    };
    let entries = match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        other => other.map_err(io)?,
    };
    let mut names = entries
        .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(io)?;
    names.sort();
    Ok(names)
}

/// The id a `<id>.json` name stands for; temp files and other names are not.
fn json_stem(name: &str) -> Option<&str> {
    name.strip_suffix(".json")
        .filter(|stem| sanitize_id(stem).is_ok())
}

/// [`Presence`] of taint under `dir`, the markers in `managed/` included.
///
/// What: any entry other than `managed/` itself is `Some`, the
/// [`REAPPLY_FAILURES_FILE`] included; a listing error is `Unknown`.
/// Test: `presence_counts_files_and_marks`,
/// `presence_is_unknown_when_a_dir_cannot_be_read`.
pub fn presence(dir: &Path) -> Presence {
    let mut seen = Presence::None;
    for sub in [dir.to_path_buf(), dir.join(MANAGED_DIR)] {
        match entry_names(&sub) {
            Err(_) => return Presence::Unknown,
            Ok(names) if names.iter().any(|name| name != MANAGED_DIR) => seen = Presence::Some,
            Ok(_) => {}
        }
    }
    seen
}

/// The managed session whose mark covers a `SessionStart` at `cwd` (D09).
///
/// Why (critic HIGH 2): the hook settles an unconfirmed `SessionStart` from
/// the user tier, so a taint must reach only the sessions of its own
/// workspace, and the record it writes must name a managed session whose
/// retire removes it.
/// What: reads each `managed/<id>.json` in id order; the first mark that
/// [`TaintMark::covers`] the canonical `cwd` names its id. A mark that cannot
/// be read covers every cwd. A listing error is [`Cover::Unknown`].
/// Test: `covering_mark_follows_the_workspace_and_fails_closed`.
pub fn covering_mark(dir: &Path, cwd: Option<&Path>) -> Cover {
    let Ok(names) = entry_names(&dir.join(MANAGED_DIR)) else {
        return Cover::Unknown;
    };
    let cwd = cwd.map(|cwd| std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf()));
    for managed in names.iter().filter_map(|name| json_stem(name)) {
        let covers = match managed_mark(dir, managed) {
            Ok(Some(mark)) => mark.covers(cwd.as_deref()),
            // Retired between the listing and the read.
            Ok(None) => false,
            Err(_) => true,
        };
        if covers {
            return Cover::Managed(managed.to_owned());
        }
    }
    Cover::None
}

/// One listed taint file: its path, and its record or why it is unreadable.
pub type ListedTaint = (PathBuf, Result<TaintRecord, String>);

/// Every taint file in `dir` with its parse, for the doctor row.
///
/// What: `*.json` entries of `dir` (not `managed/`), each with its record or
/// the reason it could not be read; temp files and [`REAPPLY_FAILURES_FILE`]
/// are skipped.
/// Test: `the_reapply_failures_record_is_not_a_taint_file`.
pub fn list_taints(dir: &Path) -> Result<Vec<ListedTaint>, TaintError> {
    let mut out = Vec::new();
    for name in entry_names(dir)? {
        if !name.ends_with(".json") {
            continue;
        }
        let path = dir.join(&name);
        let parsed = std::fs::read(&path)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()));
        out.push((path, parsed));
    }
    Ok(out)
}

/// The managed ids that carry a marker in `dir`.
pub fn marked_managed(dir: &Path) -> Result<Vec<String>, TaintError> {
    Ok(entry_names(&dir.join(MANAGED_DIR))?
        .iter()
        .filter_map(|name| json_stem(name).map(str::to_owned))
        .collect())
}

/// One other live mark a retire may hand a file to: its id, and its
/// workspace, or `None` when the mark cannot be read.
type OtherMark = (String, Option<PathBuf>);

/// Every live mark but `managed`'s, in id order, read once per retire.
///
/// What: a mark retired between the listing and the read is skipped; marks
/// that cannot be listed are an `Err`.
fn other_marks(dir: &Path, managed: &str) -> Result<Vec<OtherMark>, TaintError> {
    let mut others = Vec::new();
    for other in marked_managed(dir)? {
        if other == managed {
            continue;
        }
        match managed_mark(dir, &other) {
            Ok(None) => {}
            Ok(Some(mark)) => others.push((other, Some(mark.workspace))),
            Err(_) => others.push((other, None)),
        }
    }
    Ok(others)
}

/// The first other mark that may cover an id whose file is scoped to
/// `scope`: one that cannot be read, or whose workspace overlaps `scope`.
///
/// #8433 C1: the file's own scope decides, never the retiring mark's
/// workspace, so a hand-off cannot narrow the set of marks that cover it.
fn heir<'a>(others: &'a [OtherMark], scope: &Path) -> Option<&'a str> {
    others
        .iter()
        .find(|(_, ws)| ws.as_deref().is_none_or(|ws| scopes_overlap(ws, scope)))
        .map(|(id, _)| id.as_str())
}

/// Retire managed session `managed`: hand off or remove its taint files,
/// then remove its mark.
///
/// Why (Dfc): a taint ends only when the managed session ends. A partial
/// retire must leave the session tainted, so the mark goes last and only
/// when every file is gone. Two marked sessions can share one Claude id, so
/// a file another mark may cover outlives this retire (#8433 B1). Retiring
/// a mark never untaints an id another live mark still covers (C1).
/// What: for each `*.json` in `dir` whose `managed_session` is `managed`,
/// rewrites it, scope unchanged, for the first other mark that may cover the
/// file's own `workspace` scope (`heir`), else removes it. Marks that cannot
/// be listed, a file that cannot be read or parsed, a record with no scope,
/// and a failed rewrite or removal each leave the file in place and make
/// the retire an `Err`; the mark is then kept. Returns how many files were
/// removed.
/// Test: `retire_removes_only_that_managed_sessions_files`,
/// `retire_hands_a_shared_id_to_the_other_tainted_session_in_its_workspace`,
/// `retire_hands_an_id_to_a_mark_nested_in_its_workspace`,
/// `retire_keeps_a_shared_id_when_another_mark_cannot_be_read`,
/// `retire_hand_off_write_failure_keeps_the_mark`,
/// `retire_never_untaints_an_id_another_live_mark_covers`,
/// `a_record_without_a_scope_is_never_untainted_by_a_retire`,
/// `retire_with_an_unreadable_own_mark_hands_the_id_to_a_covering_mark`,
/// `stop_and_decommission_retire_taint`.
pub fn retire(dir: &Path, managed: &str) -> Result<usize, TaintError> {
    let marker = managed_path(dir, managed)?;
    let taints = list_taints(dir)?;
    let owns_one = taints
        .iter()
        .any(|(_, parsed)| matches!(parsed, Ok(r) if r.managed_session == managed));
    // #8433 C1: read the other marks once; each file picks its heir by scope.
    let others = if owns_one {
        other_marks(dir, managed)?
    } else {
        Vec::new()
    };
    let mut removed = 0;
    let mut first_err = None;
    for (path, parsed) in taints {
        let record = match parsed {
            Ok(record) if record.managed_session != managed => continue,
            Ok(record) => record,
            Err(reason) => {
                first_err.get_or_insert(TaintError::Malformed { path, reason });
                continue;
            }
        };
        // #8433 C1: no scope means no retire can prove nothing else covers it.
        let Some(scope) = record.workspace.as_deref() else {
            first_err.get_or_insert(TaintError::Unscoped { path });
            continue;
        };
        match heir(&others, scope) {
            Some(heir_id) => {
                let handed = TaintRecord {
                    managed_session: heir_id.to_owned(),
                    ..record
                };
                if let Err(e) = write_taint(dir, &handed) {
                    first_err.get_or_insert(e);
                }
            }
            None => match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(source) => {
                    first_err.get_or_insert(TaintError::Io { path, source });
                }
            },
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
thread_local! {
    /// The step [`at_handshake_gap`] runs once at the next handshake gap.
    static HANDSHAKE_GAP: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

/// Test seam (#9877): run `step` once, on this thread, at the next gap
/// between the two halves of the seen-id handshake ([`reapply`] or
/// [`taint_managed_session`]), to force one interleaving of the two.
#[cfg(test)]
pub(crate) fn at_handshake_gap(step: impl FnOnce() + 'static) {
    HANDSHAKE_GAP.with(|gap| *gap.borrow_mut() = Some(Box::new(step)));
}

/// The gap between the two halves of the seen-id handshake; a no-op outside
/// a test that armed [`at_handshake_gap`].
fn handshake_gap() {
    #[cfg(test)]
    if let Some(step) = HANDSHAKE_GAP.with(|gap| gap.borrow_mut().take()) {
        step();
    }
}

#[cfg(test)]
#[path = "channels_taint_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "channels_taint_seen_tests.rs"]
mod seen_tests;
