//! The daemon's channel audit log, `channels/audit.jsonl` (#9832, #8433 P3).
//!
//! Why: an operator must be able to see when the routes in effect changed,
//! at which commit, and who changed them. This is a Fail-Open Check site:
//! a route change that cannot be recorded must not go live (#8433 D5).
//! What: [`change_records`] decides whether a refresh changed the routes and
//! builds one record per changed project file; [`RouteAudit`] writes them
//! for `LoaderSource` before it swaps loaders. [`ChannelAudit`] is the
//! inbound-drop and egress-refusal audit P4 and P5 wire. Every line is a
//! trusty-channels `AuditRecord`, which has no field for message text, in
//! one owner-only file beside `questions.jsonl`.
//! Test: `audit_log_tests.rs`, and the route-change arms in
//! `snapshot_audit_tests.rs`.

use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use trusty_channels::audit::{AuditEvent, AuditLog, AuditRecord, StateError, clip};
use trusty_channels::policy::{
    Channel, CommitAuthor, FileState, FileStatus, GateError, LoadReport, MessageKind, commit_author,
};

/// The author a record names when git cannot say (#8433 recheck).
pub const UNKNOWN_AUTHOR: &str = "unknown (inferred)";

/// Looks up who last changed a project's routes at a commit.
pub type AuthorLookup = Arc<dyn Fn(&Path, &str) -> Result<CommitAuthor, GateError> + Send + Sync>;

/// Appends a batch of records to the log at a path.
pub type AuditAppend = Arc<dyn Fn(&Path, &[AuditRecord]) -> Result<(), StateError> + Send + Sync>;

/// The audit log under `home`:
/// `~/.trusty-tools/trusty-mpm/channels/audit.jsonl`.
pub fn audit_path(home: &Path) -> PathBuf {
    trusty_common::crate_config::crate_config_dir_at(
        home,
        crate::core::trusty_tools_config::CRATE_NAME,
    )
    .join("channels")
    .join("audit.jsonl")
}

/// Where a route change is recorded, and how.
///
/// Why: #8433 D5: `LoaderSource` must write a route change before the new
/// routes go live, and a test must make that write fail or wait.
/// What: the log path (`None` when home is unknown, so every write fails),
/// the author lookup and the append. The daemon's uses git's author
/// ([`commit_author`]) and [`append`].
/// Test: `audit_write_failure_keeps_old_loader_and_fingerprint`,
/// `routes_not_served_until_record_appended`.
#[derive(Clone)]
pub struct RouteAudit {
    path: Option<PathBuf>,
    author: AuthorLookup,
    append: AuditAppend,
}

impl std::fmt::Debug for RouteAudit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteAudit")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl RouteAudit {
    /// The daemon's audit: [`audit_path`] under `home`, git's author, and
    /// [`append`].
    pub fn for_home(home: Option<&Path>) -> Self {
        Self::with(
            home.map(audit_path),
            Arc::new(commit_author),
            Arc::new(append),
        )
    }

    /// An audit with its own lookup and append (a test's seam).
    pub(crate) fn with(path: Option<PathBuf>, author: AuthorLookup, append: AuditAppend) -> Self {
        Self {
            path,
            author,
            append,
        }
    }

    /// Record the change from `old` to `new`, if the routes changed.
    ///
    /// Why: #8433 D5: the caller swaps in `new` only after this returns `Ok`.
    /// What: [`change_records`]; when it names records, appends them all at
    /// once. `Ok` when nothing changed or the append is durable.
    /// Test: `audit_write_failure_keeps_old_loader_and_fingerprint`,
    /// `first_load_audit_failure_stays_never_loaded`,
    /// `unchanged_fingerprint_writes_no_record`.
    pub(crate) fn record_change(
        &self,
        old: Option<&LoadReport>,
        new: &LoadReport,
    ) -> Result<(), StateError> {
        let Some(records) = change_records(old, new, &self.author) else {
            return Ok(());
        };
        let path = self.path.as_deref().ok_or_else(no_home)?;
        (self.append)(path, &records)
    }
}

/// The records a refresh from `old` to `new` must write, or `None` when the
/// routes in effect did not change.
///
/// Why: #8433 D5: a change is what reaches enforcement, the policy or the
/// deny, not a fingerprint move (an unrelated commit moves it too). The
/// first load is recorded: a change made while the daemon was down reaches
/// enforcement only through it.
/// What: `None` when `old` and `new` have equal policy and `denied`. Else
/// one record per project file whose state or verified commit differs from
/// `old` (every file on a first load), one per file that left the load,
/// or, when no file moved (a host change), one with no project. Each names
/// the file's verified commit and its author at that commit as
/// `"<name> (inferred)"` with `committed_at`; no commit or a failed lookup
/// gives [`UNKNOWN_AUTHOR`]. Nothing is cached between records.
/// Test: `route_change_is_logged_with_author_and_time`,
/// `first_load_is_recorded`, `unrelated_commit_writes_no_record`,
/// `author_lookup_failure_records_unknown_inferred`,
/// `missing_commit_records_unknown_author`,
/// `host_only_change_records_one_unattributed_line`.
pub fn change_records(
    old: Option<&LoadReport>,
    new: &LoadReport,
    author: &AuthorLookup,
) -> Option<Vec<AuditRecord>> {
    if old.is_some_and(|o| o.policy == new.policy && o.denied == new.denied) {
        return None;
    }
    let reason = if old.is_some() {
        "routes_changed"
    } else {
        "first_load"
    };
    let before = |file: &Path| old.and_then(|o| o.per_file.iter().find(|s| s.file == file));
    let mut records: Vec<AuditRecord> = new
        .per_file
        .iter()
        .filter(|s| {
            before(&s.file)
                .is_none_or(|b| b.state != s.state || b.verified_commit != s.verified_commit)
        })
        .map(|s| file_record(reason, s, author))
        .collect();
    let gone = old
        .map(|o| o.per_file.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|b| !new.per_file.iter().any(|s| s.file == b.file));
    for b in gone {
        let mut record = route_changed(reason);
        record.project = Some(clip(&b.project_dir.display().to_string()));
        record.routes = Some(0);
        records.push(record);
    }
    if records.is_empty() {
        records.push(route_changed(reason));
    }
    Some(records)
}

/// A route-change record with no project, by [`UNKNOWN_AUTHOR`].
fn route_changed(reason: &'static str) -> AuditRecord {
    let mut record = AuditRecord::new(AuditEvent::RouteChanged, reason);
    record.author = Some(UNKNOWN_AUTHOR.into());
    record
}

/// One project file's record: project, route count, commit and author.
fn file_record(reason: &'static str, s: &FileStatus, author: &AuthorLookup) -> AuditRecord {
    let mut record = route_changed(reason);
    record.project = Some(clip(&s.project_dir.display().to_string()));
    record.routes = match s.state {
        FileState::Effective { routes } => Some(routes),
        // Last-good routes stay; this load did not count them.
        FileState::Stale => None,
        _ => Some(0),
    };
    let Some(commit) = s.verified_commit.as_deref() else {
        return record;
    };
    record.commit = Some(clip(commit));
    // #9832 A1: the lookup's own failure is recorded as unknown, never fatal.
    match author(&s.project_dir, commit) {
        Ok(found) => {
            record.author = Some(format!("{} (inferred)", clip(&found.name)));
            record.committed_at = chrono::DateTime::from_timestamp(found.time, 0)
                .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
        }
        Err(error) => {
            tracing::warn!(%error, project = %s.project_dir.display(), "route change author unknown");
        }
    }
    record
}

/// The error a write with no home returns.
fn no_home() -> StateError {
    StateError::Io {
        path: PathBuf::new(),
        reason: "home directory unknown; no channel audit log".into(),
    }
}

/// Append `records` to the log at `path`, durably, owner-only.
///
/// Why: #8433 D5: a route change is on disk before it goes live; the log
/// names projects and authors, so other users must not read it (A3).
/// What: the #8545 home-write fence, the directory created 0700, then one
/// trusty-channels owner-only append: the file created 0600 (an existing
/// one narrowed), every line in one write, then `sync_data`.
/// Test: `audit_file_is_0600_and_dir_0700`, `home_write_fence_is_called`.
pub fn append(path: &Path, records: &[AuditRecord]) -> Result<(), StateError> {
    // #8545: a test must never write the operator's home.
    crate::core::home_write_fence::check(path);
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|e| StateError::Io {
            path: dir.to_path_buf(),
            reason: e.kind().to_string(),
        })?;
    AuditLog::owner_only(path).record_all(records)
}

/// The daemon's inbound-drop and egress-refusal audit, for P4 and P5.
///
/// Why: #8433 D1: the call sites are wired by P4 (egress) and P5
/// (inbound); they share the record and the file with the route-change log.
/// What: one line per event, with no text field. The caller drops the
/// message or refuses the send whatever the result; an `Err` only says the
/// line was not written. No home makes every write an `Err`.
/// Test: `unknown_sender_logs_only_sender_time_length`,
/// `refused_send_is_logged`, `channel_audit_with_no_home_is_an_error`.
#[derive(Debug, Clone)]
pub struct ChannelAudit {
    path: Option<PathBuf>,
}

impl ChannelAudit {
    /// The audit at [`audit_path`] under `home`.
    pub fn for_home(home: Option<&Path>) -> Self {
        Self {
            path: home.map(audit_path),
        }
    }

    /// An inbound message from `sender`, `length` bytes long, was dropped
    /// for `reason`.
    pub fn inbound_dropped(
        &self,
        channel: Channel,
        sender: &str,
        length: usize,
        reason: &'static str,
    ) -> Result<(), StateError> {
        let mut record = AuditRecord::new(AuditEvent::InboundDropped, reason);
        record.channel = Some(channel.as_str());
        record.sender = Some(clip(sender));
        record.length = Some(length);
        self.write(&record)
    }

    /// RED STUB (#9873 S3): writes the wrong event; the next commit fixes it.
    pub fn rate_limited(
        &self,
        channel: Channel,
        sender: &str,
        length: usize,
        reason: &'static str,
    ) -> Result<(), StateError> {
        self.inbound_dropped(channel, sender, length, reason)
    }

    /// A `kind` send to `to` was refused for `reason`.
    pub fn send_refused(
        &self,
        channel: Channel,
        to: &str,
        kind: MessageKind,
        reason: &'static str,
    ) -> Result<(), StateError> {
        let mut record = AuditRecord::new(AuditEvent::SendRefused, reason);
        record.channel = Some(channel.as_str());
        record.to = Some(clip(to));
        record.kind = Some(kind.as_str());
        self.write(&record)
    }

    fn write(&self, record: &AuditRecord) -> Result<(), StateError> {
        let path = self.path.as_deref().ok_or_else(no_home)?;
        append(path, std::slice::from_ref(record))
    }
}

#[cfg(test)]
#[path = "audit_log_tests.rs"]
mod tests;
