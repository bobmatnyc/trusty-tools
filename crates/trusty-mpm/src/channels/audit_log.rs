//! The daemon's channel audit log, `channels/audit.jsonl` (#9832, #8433 P3).
//!
//! Why: an operator must be able to see when the routes in effect changed,
//! at which commit, and who changed them. This is a Fail-Open Check site:
//! a route change that cannot be recorded must not go live (#8433 D5).
//! What: seam; see the implementation commit.
//! Test: `audit_log_tests.rs`, and the route-change arms of
//! `snapshot_tests.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use trusty_channels::audit::{AuditRecord, StateError};
use trusty_channels::policy::{
    Channel, CommitAuthor, GateError, LoadReport, MessageKind, commit_author,
};

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
/// Why: seam; see the implementation commit.
/// What: the log path (`None` when home is unknown), the author lookup and
/// the append.
/// Test: `audit_write_failure_keeps_old_loader_and_fingerprint`.
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

    /// The author lookup.
    pub(crate) fn author(&self) -> &AuthorLookup {
        &self.author
    }

    /// Append `records` durably.
    pub(crate) fn write(&self, records: &[AuditRecord]) -> Result<(), StateError> {
        let _ = (records, &self.path, &self.append);
        Ok(())
    }
}

/// The records a refresh from `old` to `new` must write, or `None` when the
/// routes in effect did not change.
///
/// Why: seam; see the implementation commit.
/// Test: `route_change_is_logged_with_author_and_time`,
/// `unchanged_fingerprint_writes_no_record`, `first_load_is_recorded`.
pub fn change_records(
    old: Option<&LoadReport>,
    new: &LoadReport,
    author: &AuthorLookup,
) -> Option<Vec<AuditRecord>> {
    let _ = (old, new, author);
    None
}

/// Append `records` to the log at `path`.
///
/// Why: seam; see the implementation commit.
/// Test: `audit_file_is_0600_and_dir_0700`, `home_write_fence_is_called`.
pub fn append(path: &Path, records: &[AuditRecord]) -> Result<(), StateError> {
    let _ = (path, records);
    Ok(())
}

/// The daemon's inbound-drop and egress-refusal audit, for P4 and P5.
///
/// Why: seam; see the implementation commit.
/// Test: `unknown_sender_logs_only_sender_time_length`, `refused_send_is_logged`.
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

    /// An inbound message was dropped.
    pub fn inbound_dropped(
        &self,
        channel: Channel,
        sender: &str,
        length: usize,
        reason: &'static str,
    ) -> Result<(), StateError> {
        let _ = (&self.path, channel, sender, length, reason);
        Ok(())
    }

    /// An outbound message was refused.
    pub fn send_refused(
        &self,
        channel: Channel,
        to: &str,
        kind: MessageKind,
        reason: &'static str,
    ) -> Result<(), StateError> {
        let _ = (&self.path, channel, to, kind, reason);
        Ok(())
    }
}
