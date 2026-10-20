//! The lean `session_context_catchup` default (#9883).
//!
//! Why: owner decision Dba — "Lean resume by default." A resume needs the
//! snapshot it resumes from and the checkout's state now. The digest (sessions
//! page, recent commits, memory titles) cost a page of context on every resume,
//! so it moved behind `full: true`, which #9883 merged with the old
//! "ignore the watermark" meaning of that flag.
//! What: [`live_git_status`], a time-bounded `git status --porcelain` in the
//! project, and [`lean_payload`], the response body built from the same
//! resolution, freshness, verify and hydration parts the full answer carries.
//! Test: `catchup_lean_tests.rs`; this file's `live_git_status_*` and
//! `lean_payload_*` tests.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use trusty_common::catchup::PausedSessionJson;

use crate::core::bounded_proc::run_bounded;
use crate::core::catchup::resolve::ResolvedSnapshot;
use crate::daemon::catchup_payload::HydrationReceipt;
use crate::daemon::catchup_superseded::SnapshotFreshness;

/// How long the live `git status` may run before it is killed.
pub(crate) const GIT_STATUS_BUDGET: Duration = Duration::from_secs(5);

/// The most porcelain entries a lean answer carries.
pub(crate) const GIT_STATUS_MAX_ENTRIES: usize = 40;

/// The `git_status` field of a lean answer: the checkout's state at call time.
///
/// Why: a snapshot's `git_context` is the state at PAUSE time; a resume acts
/// on the state now. A probe that hangs or fails must say so, never read as a
/// clean tree.
/// What: up to [`GIT_STATUS_MAX_ENTRIES`] `git status --porcelain` lines,
/// verbatim, with the full count. `available: false` carries `error` and no
/// entries.
/// Test: `live_git_status_lists_a_dirty_tree_verbatim`,
/// `live_git_status_is_unavailable_outside_a_checkout`,
/// `lean_payload_bounds_the_status_entries`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct LiveGitStatus {
    /// Whether `git status` answered successfully within its budget.
    pub(crate) available: bool,
    /// The first porcelain lines, untrimmed — the leading column is the status.
    pub(crate) entries: Vec<String>,
    /// How many porcelain lines `git status` printed.
    pub(crate) entries_total: usize,
    /// Whether `entries` stops short of `entries_total`.
    pub(crate) truncated: bool,
    /// Why the status is unavailable, when it is.
    pub(crate) error: Option<String>,
}

impl LiveGitStatus {
    /// A status that could not be read, with the reason.
    pub(crate) fn unavailable(reason: impl std::fmt::Display) -> Self {
        Self {
            error: Some(format!("unavailable: {reason}")),
            ..Self::default()
        }
    }

    /// Porcelain output bounded to `max` entries.
    fn from_porcelain(stdout: &str, max: usize) -> Self {
        let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();
        Self {
            available: true,
            entries: lines.iter().take(max).map(|l| (*l).to_string()).collect(),
            entries_total: lines.len(),
            truncated: lines.len() > max,
            error: None,
        }
    }
}

/// Read `git status --porcelain` in `project_dir`, killed after `budget`.
///
/// Why: a lean resume shows the live tree beside the pause-time one, and a
/// catch-up must never hang on a wedged git (an index lock, a slow
/// filesystem).
/// What: `git --no-optional-locks status --porcelain -- .` through
/// [`run_bounded`], so it neither takes nor waits on the index lock, lists
/// only paths under `project_dir`, and its process group is killed at
/// `budget`. A spawn failure, timeout or non-zero exit is
/// [`LiveGitStatus::unavailable`] with the reason.
/// Test: `live_git_status_lists_a_dirty_tree_verbatim`,
/// `live_git_status_lists_only_paths_under_a_subdirectory_project`,
/// `live_git_status_is_unavailable_outside_a_checkout`.
pub(crate) fn live_git_status(project_dir: &Path, budget: Duration) -> LiveGitStatus {
    let mut cmd = trusty_common::git::command_in(project_dir);
    // #9883: `-- .` scopes a subdirectory project to its own tree.
    cmd.args(["--no-optional-locks", "status", "--porcelain", "--", "."])
        .stdin(Stdio::null());
    match run_bounded(cmd, budget) {
        Ok(out) if out.status.success() => {
            LiveGitStatus::from_porcelain(&out.stdout, GIT_STATUS_MAX_ENTRIES)
        }
        Ok(out) => {
            let why = out.stderr.lines().next().unwrap_or("").trim().to_string();
            LiveGitStatus::unavailable(format!("git status exited {}: {why}", out.status))
        }
        Err(e) => LiveGitStatus::unavailable(format!("git status {e}")),
    }
}

/// [`live_git_status`] off the async executor.
pub(crate) async fn live_git_status_async(project_dir: &Path) -> LiveGitStatus {
    let dir = project_dir.to_path_buf();
    tokio::task::spawn_blocking(move || live_git_status(&dir, GIT_STATUS_BUDGET))
        .await
        .unwrap_or_else(|e| LiveGitStatus::unavailable(format!("git status probe failed: {e}")))
}

/// The parts of a lean answer, gathered by `session_context_catchup_in`.
pub(crate) struct LeanParts {
    /// The snapshot resolved for the caller, unchanged from the full answer.
    pub(crate) resolved: Option<ResolvedSnapshot>,
    /// That snapshot's recorded fields, when it could be read.
    pub(crate) resolved_session: Option<PausedSessionJson>,
    /// #8408: why the tmux-session route was not tried, when it was not.
    pub(crate) note: Option<&'static str>,
    /// #7830: the session-ref hydration receipt.
    pub(crate) session_refs: HydrationReceipt,
    /// #7501: whether the checkout moved past the snapshot.
    pub(crate) freshness: SnapshotFreshness,
    /// #9390: the resume verify report.
    pub(crate) resume_verify: Value,
    /// #9883: the live tree.
    pub(crate) git_status: LiveGitStatus,
}

/// Shape a lean `session_context_catchup` answer.
///
/// Why: the default answer drops the digest, so it must say it is lean — a
/// caller reading an absent `sessions` key cannot tell "lean" from "broken".
/// What: `digest: "lean"`, the resolution keys the full answer carries
/// (`resolved_snapshot`, `resolved_via`, `resolved_snapshot_superseded`,
/// `commits_since_snapshot(_total)`, `resume_verify`, `session_refs`,
/// `watermark_advanced`, `resolved_note` when set) plus `resolved_session` and
/// `git_status`. No `sessions`, `recent_commits` or `recent_memory`.
/// Test: `lean_payload_carries_no_digest_keys`,
/// `the_default_returns_the_resolved_snapshot_and_live_git_status_only`.
pub(crate) fn lean_payload(parts: LeanParts) -> Value {
    let (snapshot, via) = match parts.resolved {
        Some(r) => (Some(r.path.display().to_string()), Some(r.via.as_str())),
        None => (None, None),
    };
    let mut body = json!({
        "digest": "lean",
        "resolved_snapshot": snapshot,
        "resolved_via": via,
        "resolved_session": parts.resolved_session,
        "resolved_snapshot_superseded": parts.freshness.superseded,
        "commits_since_snapshot": parts.freshness.commits_since,
        "commits_since_snapshot_total": parts.freshness.total_since,
        "resume_verify": parts.resume_verify,
        "git_status": parts.git_status,
        "watermark_advanced": false,
        "session_refs": parts.session_refs.to_json(),
    });
    if let (Some(note), Some(map)) = (parts.note, body.as_object_mut()) {
        map.insert("resolved_note".into(), json!(note));
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `git -C <dir>` with no global or system config (#9883): a host's
    /// `commit.gpgsign` or `status.showUntrackedFiles` must not reach a fixture.
    fn fixture_git(dir: &Path) -> std::process::Command {
        let mut cmd = trusty_common::git::command_in(dir);
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        cmd
    }

    /// An empty checkout whose local config pins what the probe reads.
    fn checkout() -> tempfile::TempDir {
        let tmp = tempfile::TempDir::new().unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.email", "lean@example.invalid"],
            vec!["config", "user.name", "Lean"],
            // The probe under test runs with host config; a local value wins.
            vec!["config", "status.showUntrackedFiles", "normal"],
        ] {
            let out = fixture_git(tmp.path()).args(&args).output().unwrap();
            assert!(out.status.success(), "fixture `git {args:?}` failed");
        }
        tmp
    }

    /// Why (#9883): the porcelain status column is leading whitespace, so the
    /// probe must hand the lines back untrimmed.
    #[test]
    fn live_git_status_lists_a_dirty_tree_verbatim() {
        let tmp = checkout();
        assert_eq!(
            live_git_status(tmp.path(), GIT_STATUS_BUDGET),
            LiveGitStatus {
                available: true,
                ..LiveGitStatus::default()
            },
            "a fresh repo is clean"
        );
        std::fs::write(tmp.path().join("new.txt"), b"n").unwrap();
        let status = live_git_status(tmp.path(), GIT_STATUS_BUDGET);
        assert!(status.available, "{status:?}");
        assert_eq!(status.entries, vec!["?? new.txt".to_string()]);
        assert_eq!(status.entries_total, 1);
        assert!(!status.truncated);
    }

    /// Why (#9883): a `project_dir` below the repo root reports its own tree,
    /// not the parent repo's — a sibling project's edits are not this resume's.
    #[test]
    fn live_git_status_lists_only_paths_under_a_subdirectory_project() {
        let tmp = checkout();
        let sub = tmp.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(tmp.path().join("outside.txt"), b"o").unwrap();
        std::fs::write(sub.join("inside.txt"), b"i").unwrap();
        let status = live_git_status(&sub, GIT_STATUS_BUDGET);
        assert!(status.available, "{status:?}");
        assert!(
            !status.entries.iter().any(|e| e.contains("outside.txt")),
            "{status:?}"
        );
        assert_eq!(status.entries_total, 1, "{status:?}");
        assert!(status.entries[0].starts_with("?? sub/"), "{status:?}");
    }

    /// Why (#9883): a failed probe must say so, never read as a clean tree.
    #[test]
    fn live_git_status_is_unavailable_outside_a_checkout() {
        let tmp = tempfile::TempDir::new().unwrap();
        let status = live_git_status(&tmp.path().join("missing"), GIT_STATUS_BUDGET);
        assert!(!status.available, "{status:?}");
        assert!(status.entries.is_empty());
        let error = status.error.expect("the reason is reported");
        assert!(error.starts_with("unavailable: "), "{error}");
    }

    /// Why (#9883): the field must stay short however dirty the tree is.
    #[test]
    fn lean_payload_bounds_the_status_entries() {
        let stdout: String = (0..GIT_STATUS_MAX_ENTRIES + 5)
            .map(|i| format!("?? f{i}.txt\n"))
            .collect();
        let status = LiveGitStatus::from_porcelain(&stdout, GIT_STATUS_MAX_ENTRIES);
        assert_eq!(status.entries.len(), GIT_STATUS_MAX_ENTRIES);
        assert_eq!(status.entries_total, GIT_STATUS_MAX_ENTRIES + 5);
        assert!(status.truncated);
    }

    /// Why (#9883): the lean answer names itself and carries no digest key.
    #[test]
    fn lean_payload_carries_no_digest_keys() {
        let body = lean_payload(LeanParts {
            resolved: None,
            resolved_session: None,
            note: None,
            session_refs: HydrationReceipt::default(),
            freshness: SnapshotFreshness::default(),
            resume_verify: Value::Null,
            git_status: LiveGitStatus::unavailable("test"),
        });
        assert_eq!(body["digest"], "lean");
        assert_eq!(body["git_status"]["available"], false);
        assert_eq!(body["git_status"]["error"], "unavailable: test");
        for key in [
            "sessions",
            "recent_commits",
            "recent_memory",
            "resolved_note",
        ] {
            assert!(body.get(key).is_none(), "{key}: {body}");
        }
        assert_eq!(body["watermark_advanced"], false);
    }
}
