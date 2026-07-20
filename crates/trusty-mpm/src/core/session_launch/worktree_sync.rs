//! Sync a per-session git worktree with its upstream branch on resume (#2647).
//!
//! Why: `WorkspaceProvisioner`'s `fetch_and_reset`
//! (`crate::provisioner::workspace`) keeps the SHARED `.base` bare checkout
//! current with `origin/main`, but the per-session worktree created off it
//! (`<project>/.base/.worktrees/<session-id>/`) was never re-synced on
//! resume — once created, its branch was frozen at that commit forever. A
//! long-lived session silently drifted from `origin/main` and re-exhibited
//! bugs already fixed upstream (e.g. a stale, since-deleted `CLAUDE.md`
//! reappearing after a later commit removed it — issue #2647's root cause 1).
//! `configure_session_branch_tracking` (issue #2189,
//! `daemon::managed_routes::inproject`) already points the session branch's
//! upstream at `origin/<default-branch>` at creation time; this module is the
//! missing piece that actually USES that tracking relationship on resume.
//! What: [`sync_worktree_with_upstream`] runs `git fetch` + a **fast-forward
//! only** merge against the configured upstream. Fast-forward-only is the
//! load-bearing safety property required by #2647: it NEVER rewrites the
//! branch's history (`git reset --hard` is never invoked), and git itself
//! refuses the merge outright if any uncommitted local change would be
//! overwritten by it — so genuine user work in this worktree is either
//! preserved untouched (files the incoming commits do not touch) or the
//! whole sync is skipped (files that collide), never silently clobbered.
//! [`self_heal_claude_md`] separately strips any legacy (#2170)
//! delegation-directive pollution from the project `CLAUDE.md` even when it
//! carries an uncommitted local diff that would otherwise block the
//! fast-forward (exactly the state issue #2647 found this session's own
//! worktree in).
//! Test: `sync_fast_forwards_tracked_file_leaves_uncommitted_user_file_untouched`,
//! `sync_is_noop_when_already_up_to_date`,
//! `sync_skips_when_no_upstream_configured`,
//! `sync_fails_open_on_conflicting_uncommitted_change`,
//! `sync_fails_open_when_branches_have_diverged_with_local_commit`,
//! `self_heal_claude_md_strips_legacy_block`,
//! `self_heal_claude_md_noop_when_clean`,
//! `self_heal_claude_md_noop_when_absent`.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Upper bound on how long [`resume_self_heal`] will wait for the git
/// fetch/merge to complete before giving up and letting resume proceed.
///
/// Why (code-critic review, #2647): the git subprocess work runs on a
/// blocking thread via `tokio::task::spawn_blocking`, but a stalled network
/// fetch (e.g. a dead remote, a hung proxy) can otherwise keep that thread —
/// and the resume flow awaiting it — blocked indefinitely. Resume must never
/// hang on network I/O.
/// What: 10 seconds, generous for a same-org `origin` fetch of a handful of
/// commits, short enough that a hung fetch cannot meaningfully delay a
/// session resume.
const SYNC_TIMEOUT: Duration = Duration::from_secs(10);

/// Outcome of one [`sync_worktree_with_upstream`] attempt.
///
/// Why: `resume_managed` logs this for observability but never treats any
/// variant as fatal — every path here is fail-open by design (see module
/// docs): a failed or skipped sync just means the worktree stays exactly as
/// it was, which is always safe.
/// What: one variant per terminal state of the sync attempt.
/// Test: each variant is produced by a corresponding unit test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// The worktree branch was already at its upstream tip — no change made.
    UpToDate,
    /// Fast-forwarded from `from` to `to` (both full commit SHAs).
    FastForwarded {
        /// The branch tip before the sync.
        from: String,
        /// The branch tip after the sync.
        to: String,
    },
    /// `workspace` is not a git worktree (no `.git`) — nothing to sync.
    NotAGitWorktree,
    /// The branch has no upstream tracking ref configured — nothing to sync
    /// against. Worktrees created before issue #2189, or created outside the
    /// `inproject` path, may legitimately land here.
    NoUpstream,
    /// `git fetch` or the fast-forward merge failed or was refused (most
    /// commonly: local uncommitted changes collide with the upstream diff,
    /// so git refuses to overwrite them). The payload is git's own stderr,
    /// trimmed. The worktree's working tree is left exactly as it was.
    Failed(String),
}

/// Fetch and fast-forward-merge `workspace`'s current branch onto its
/// configured upstream, never touching commits or uncommitted changes that
/// would require anything OTHER than a clean fast-forward.
///
/// Why: see module docs — this is the resume-time counterpart to
/// `WorkspaceProvisioner::fetch_and_reset` for the shared base checkout,
/// scoped to a single session worktree and safe to run against a workspace
/// that may carry uncommitted operator edits.
/// What: resolves `@{upstream}` for the current branch; if absent, returns
/// [`SyncOutcome::NoUpstream`]. Otherwise runs `git fetch <remote>` (the
/// remote named by the upstream ref, e.g. `origin`), then
/// `git merge --ff-only <upstream>`. A non-zero exit from either step yields
/// [`SyncOutcome::Failed`] with git's stderr; the caller MUST treat this as
/// non-fatal (see `resume_managed`).
/// Test: `sync_fast_forwards_tracked_file_leaves_uncommitted_user_file_untouched`,
/// `sync_is_noop_when_already_up_to_date`,
/// `sync_skips_when_no_upstream_configured`,
/// `sync_fails_open_on_conflicting_uncommitted_change`,
/// `sync_fails_open_when_branches_have_diverged_with_local_commit`.
pub fn sync_worktree_with_upstream(workspace: &Path) -> SyncOutcome {
    if !workspace.join(".git").exists() {
        return SyncOutcome::NotAGitWorktree;
    }

    let Some(upstream) = git_stdout(
        workspace,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
    ) else {
        return SyncOutcome::NoUpstream;
    };

    let Some(remote) = upstream.split('/').next().filter(|s| !s.is_empty()) else {
        return SyncOutcome::NoUpstream;
    };

    let before = git_stdout(workspace, &["rev-parse", "HEAD"]).unwrap_or_default();

    // Defense in depth (code-critic review, #2647) alongside the outer
    // `SYNC_TIMEOUT`: abort the fetch itself if the transfer stalls below
    // 1000 bytes/sec for more than 5 seconds, so a slow-but-not-dead remote
    // cannot eat most of the outer timeout budget on the network layer alone.
    if let Err(stderr) = git_run(
        workspace,
        &[
            "-c",
            "http.lowSpeedLimit=1000",
            "-c",
            "http.lowSpeedTime=5",
            "fetch",
            remote,
        ],
    ) {
        return SyncOutcome::Failed(format!("git fetch failed: {stderr}"));
    }

    if let Err(stderr) = git_run(workspace, &["merge", "--ff-only", &upstream]) {
        return SyncOutcome::Failed(stderr);
    }

    let after = git_stdout(workspace, &["rev-parse", "HEAD"]).unwrap_or_default();
    if before == after {
        SyncOutcome::UpToDate
    } else {
        SyncOutcome::FastForwarded {
            from: before,
            to: after,
        }
    }
}

/// Strip the legacy (#2170) delegation-directive block from the project's
/// `CLAUDE.md`, if present — self-healing pollution left by pre-#2170
/// worktrees even when it carries an uncommitted local diff that would
/// otherwise block [`sync_worktree_with_upstream`]'s fast-forward.
///
/// Why (issue #2647): a worktree provisioned before #2170 shipped can carry
/// the legacy block as an UNCOMMITTED local edit (exactly the state #2647
/// found this session's own worktree in). `git merge --ff-only` correctly
/// refuses to touch a file with an uncommitted diff (see module docs), so the
/// git-level sync alone cannot self-heal this specific pollution.
/// `strip_delegation_block` is already proven byte-identical-safe on clean
/// input (issue #2170); calling it unconditionally on resume is a pure
/// content edit (no git operation), so it applies regardless of the
/// worktree's git state.
/// What: reads `<workspace>/CLAUDE.md` (a no-op, returns `false`, if
/// absent/unreadable), strips the fenced block via
/// [`crate::core::instruction_pipeline::strip_delegation_block`], and writes
/// back ONLY when the content actually changed. Returns `true` iff a write
/// happened.
/// Test: `self_heal_claude_md_strips_legacy_block`,
/// `self_heal_claude_md_noop_when_clean`,
/// `self_heal_claude_md_noop_when_absent`.
pub fn self_heal_claude_md(workspace: &Path) -> bool {
    let path = workspace.join("CLAUDE.md");
    let Ok(original) = std::fs::read_to_string(&path) else {
        return false;
    };
    let cleaned = crate::core::instruction_pipeline::strip_delegation_block(&original);
    if cleaned == original {
        return false;
    }
    std::fs::write(&path, cleaned).is_ok()
}

/// Best-effort resume-time self-heal: sync `workspace`'s worktree branch to
/// its upstream (fast-forward only) and strip legacy delegation-directive
/// pollution from `CLAUDE.md`, logging outcomes but never failing.
///
/// Why: the call site (`daemon::managed_routes::lifecycle::resume_managed`)
/// must stay a one-line call rather than inlining the
/// [`SyncOutcome`] match + logging itself — that inline form is what pushed
/// `lifecycle.rs` over its frozen SLOC budget during #2647 review.
/// Centralising the log-and-continue wiring here keeps the call site small
/// and keeps this module the single place that knows how to interpret a
/// [`SyncOutcome`]. This function is `async` (code-critic review, #2647):
/// `resume_managed` runs on a tokio worker thread, and the git fetch/merge
/// in [`sync_worktree_with_upstream`] are blocking subprocess calls that can
/// stall on network I/O — running them directly on the async task would
/// block that worker thread (and, transitively, resume) for as long as the
/// network hangs.
/// What: runs [`sync_worktree_with_upstream`] on a `spawn_blocking` thread,
/// bounded by [`SYNC_TIMEOUT`] — a timeout or a panicked blocking task both
/// degrade to `SyncOutcome::Failed` and are logged exactly like any other
/// sync failure (fail-open; resume always proceeds). Then calls
/// [`self_heal_claude_md`] directly (a single small `CLAUDE.md` read/write,
/// no git subprocess, no network — not a resume-hang risk). Logs a
/// `FastForwarded` or `Failed` sync outcome (and a successful self-heal) at
/// `info`/`warn` via `tracing`; steady states
/// (`UpToDate`/`NotAGitWorktree`/`NoUpstream`) are not logged. `session_id`
/// is included in every log line for correlation; callers pass
/// `record.id.to_string()`.
/// Test: covered indirectly via [`sync_worktree_with_upstream`]'s and
/// [`self_heal_claude_md`]'s own unit tests above — this wrapper adds only
/// async/timeout plumbing and logging, no independent branching logic.
pub async fn resume_self_heal(workspace: &Path, session_id: &str) {
    match sync_worktree_with_upstream_bounded(workspace).await {
        SyncOutcome::FastForwarded { from, to } => {
            tracing::info!(id = %session_id, %from, %to, "resume: worktree fast-forwarded to upstream");
        }
        SyncOutcome::Failed(reason) => {
            tracing::warn!(id = %session_id, "resume: worktree/upstream sync skipped (non-fatal): {reason}");
        }
        SyncOutcome::UpToDate | SyncOutcome::NotAGitWorktree | SyncOutcome::NoUpstream => {}
    }
    if self_heal_claude_md(workspace) {
        tracing::info!(id = %session_id, "resume: stripped legacy delegation-directive pollution from CLAUDE.md");
    }
}

/// Run [`sync_worktree_with_upstream`] on a blocking thread-pool thread,
/// bounded by [`SYNC_TIMEOUT`] so a stalled fetch can never hang the caller.
///
/// Why: see [`resume_self_heal`]'s doc comment — this is the seam that keeps
/// the blocking git subprocess work off the async task and off the resume
/// critical path past a fixed budget.
/// What: `tokio::task::spawn_blocking` runs the sync on a dedicated thread;
/// `tokio::time::timeout(SYNC_TIMEOUT, ...)` races it against the deadline.
/// A timeout or a `JoinError` (blocking-task panic) both degrade to
/// `SyncOutcome::Failed` with a descriptive message — never propagated as an
/// error to the caller, matching the fail-open contract of every other
/// variant. Note: on timeout the abandoned blocking task keeps running to
/// completion in the background (it cannot be forcibly killed mid-syscall);
/// this only bounds how long `resume_self_heal` itself waits.
/// Test: exercised transitively by every `sync_*` test above (the bounded
/// wrapper adds no branching beyond the timeout race, which is impractical
/// to trigger deterministically in a fast unit test without a fake slow git
/// binary).
async fn sync_worktree_with_upstream_bounded(workspace: &Path) -> SyncOutcome {
    let workspace = workspace.to_path_buf();
    let task = tokio::task::spawn_blocking(move || sync_worktree_with_upstream(&workspace));
    match tokio::time::timeout(SYNC_TIMEOUT, task).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(join_err)) => SyncOutcome::Failed(format!("sync task panicked: {join_err}")),
        Err(_elapsed) => SyncOutcome::Failed(format!(
            "worktree sync timed out after {}s",
            SYNC_TIMEOUT.as_secs()
        )),
    }
}

/// Run `git -C <workspace> <args>`, returning trimmed stdout on success or
/// `None` on any failure (missing binary, non-zero exit, empty output).
fn git_stdout(workspace: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Run `git -C <workspace> <args>`, returning `Ok(())` on success or
/// `Err(stderr)` (trimmed) on any failure.
fn git_run(workspace: &Path, args: &[&str]) -> Result<(), String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .map_err(|e| format!("git exec failed: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Real, disposable git fixture mirroring the shape #2647 describes:
    /// a bare `origin`, a `base` clone of it (stands in for the shared
    /// `.base` checkout), and a `worktree` created off `base` with upstream
    /// tracking configured exactly like `configure_session_branch_tracking`
    /// (issue #2189) sets it up in production.
    ///
    /// Returns `None` (callers must skip, not fail) when `git` is
    /// unavailable in this environment — mirrors the established pattern in
    /// `session_manager::decommission_worktree_tests`.
    struct Fixture {
        _origin_dir: TempDir,
        _base_dir: TempDir,
        _seed_dir: TempDir,
        worktree_dir: TempDir,
        origin: std::path::PathBuf,
        seed: std::path::PathBuf,
    }

    fn git_ok(dir: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn configure_identity(dir: &Path) {
        let _ = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["config", "user.email", "ci@test.invalid"])
            .output();
        let _ = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["config", "user.name", "CI"])
            .output();
    }

    /// Not purely mechanical (#3390): the four `TempDir::new().ok()?` sites
    /// below became `crate::test_support::hermetic_temp_dir()` calls with the
    /// trailing `?` dropped — `hermetic_temp_dir()` returns `TempDir`
    /// directly (it panics via `.expect(...)` internally on temp-dir-creation
    /// failure) rather than `Option<TempDir>`. Practically equivalent: OS
    /// temp-directory creation failing at all is exceedingly rare (disk full
    /// / permission denied), and unlike the git-unavailable checks below
    /// (which legitimately `return None` to skip cleanly), an actual failure
    /// to create a scratch directory is a real test-environment problem worth
    /// a hard failure rather than a silent skip.
    fn build_fixture() -> Option<Fixture> {
        let origin_dir = crate::test_support::hermetic_temp_dir();
        let origin = origin_dir.path().to_path_buf();
        if !git_ok(&origin, &["init", "--bare"]) {
            eprintln!("worktree_sync tests: git unavailable, skipping");
            return None;
        }

        // Seed clone: writes the initial commit (CLAUDE.md + OTHER.md) and
        // pushes it to `main` on the bare origin, then anchors the bare
        // repo's HEAD symref at `main` so a later `git clone` resolves a
        // real default branch instead of an empty/detached one.
        let seed_dir = crate::test_support::hermetic_temp_dir();
        let seed = seed_dir.path().to_path_buf();
        if !git_ok(&seed, &["init"]) {
            return None;
        }
        configure_identity(&seed);
        if !git_ok(&seed, &["checkout", "-b", "main"]) {
            return None;
        }
        std::fs::write(seed.join("CLAUDE.md"), "old content\n").ok()?;
        std::fs::write(seed.join("OTHER.md"), "other v1\n").ok()?;
        if !git_ok(&seed, &["add", "-A"]) {
            return None;
        }
        if !git_ok(&seed, &["commit", "-m", "init"]) {
            return None;
        }
        if !git_ok(
            &seed,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        ) {
            return None;
        }
        if !git_ok(&seed, &["push", "origin", "main"]) {
            return None;
        }
        if !git_ok(&origin, &["symbolic-ref", "HEAD", "refs/heads/main"]) {
            return None;
        }

        // Base clone: stands in for the shared `.base` checkout.
        let base_dir = crate::test_support::hermetic_temp_dir();
        let base = base_dir.path().to_path_buf();
        // `TempDir::new` already created the directory; `git clone` refuses
        // to clone into a non-empty existing dir in some git versions, so
        // remove it first and let clone recreate it.
        let _ = std::fs::remove_dir(&base);
        if !git_ok(
            Path::new("."),
            &["clone", origin.to_str().unwrap(), base.to_str().unwrap()],
        ) {
            return None;
        }
        configure_identity(&base);

        // Session worktree, mirroring `create_session_worktree` +
        // `configure_session_branch_tracking` (issue #2189): a new branch
        // checked out from `base`'s HEAD, with its upstream explicitly
        // pointed at `origin/main`.
        let worktree_dir = crate::test_support::hermetic_temp_dir();
        let worktree = worktree_dir.path().to_path_buf();
        let _ = std::fs::remove_dir(&worktree);
        if !git_ok(
            &base,
            &[
                "worktree",
                "add",
                "-b",
                "session-branch",
                worktree.to_str().unwrap(),
            ],
        ) {
            return None;
        }
        configure_identity(&worktree);
        if !git_ok(
            &worktree,
            &["branch", "--set-upstream-to=origin/main", "session-branch"],
        ) {
            return None;
        }

        Some(Fixture {
            _origin_dir: origin_dir,
            _base_dir: base_dir,
            _seed_dir: seed_dir,
            worktree_dir,
            origin,
            seed,
        })
    }

    /// Push a new commit to `origin`'s `main` via the seed clone, updating
    /// `CLAUDE.md` to `new_content`. Mirrors an upstream fix (e.g. #2300)
    /// landing after this session's worktree was created.
    fn push_claude_md_update(fx: &Fixture, new_content: &str) {
        assert!(git_ok(&fx.seed, &["pull", "origin", "main"]));
        std::fs::write(fx.seed.join("CLAUDE.md"), new_content).unwrap();
        assert!(git_ok(&fx.seed, &["add", "-A"]));
        assert!(git_ok(&fx.seed, &["commit", "-m", "update CLAUDE.md"]));
        assert!(git_ok(&fx.seed, &["push", "origin", "main"]));
        let _ = &fx.origin; // origin is referenced only via the seed's remote
    }

    #[test]
    fn sync_fast_forwards_tracked_file_leaves_uncommitted_user_file_untouched() {
        let Some(fx) = build_fixture() else {
            return;
        };
        let worktree = fx.worktree_dir.path();

        // Simulate user work in progress: an uncommitted edit to a file the
        // upstream commit below does NOT touch.
        std::fs::write(worktree.join("OTHER.md"), "other v1 + user edit\n").unwrap();

        // Simulate an upstream fix landing (e.g. #2300) while this worktree
        // was never fetched.
        push_claude_md_update(&fx, "new content\n");

        let outcome = sync_worktree_with_upstream(worktree);
        assert!(
            matches!(outcome, SyncOutcome::FastForwarded { .. }),
            "expected a fast-forward, got {outcome:?}"
        );

        assert_eq!(
            std::fs::read_to_string(worktree.join("CLAUDE.md")).unwrap(),
            "new content\n",
            "tm-tracked CLAUDE.md must refresh to the current origin/main content"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("OTHER.md")).unwrap(),
            "other v1 + user edit\n",
            "uncommitted user work in an untouched file must survive the sync"
        );
    }

    #[test]
    fn sync_is_noop_when_already_up_to_date() {
        let Some(fx) = build_fixture() else {
            return;
        };
        let worktree = fx.worktree_dir.path();

        let first = sync_worktree_with_upstream(worktree);
        assert_eq!(first, SyncOutcome::UpToDate);
        let second = sync_worktree_with_upstream(worktree);
        assert_eq!(second, SyncOutcome::UpToDate);
    }

    #[test]
    fn sync_skips_when_no_upstream_configured() {
        let tmp = crate::test_support::hermetic_temp_dir();
        let dir = tmp.path();
        if !git_ok(dir, &["init"]) {
            return;
        }
        configure_identity(dir);
        std::fs::write(dir.join("f.txt"), "x").unwrap();
        if !git_ok(dir, &["add", "-A"]) || !git_ok(dir, &["commit", "-m", "init"]) {
            return;
        }

        assert_eq!(sync_worktree_with_upstream(dir), SyncOutcome::NoUpstream);
    }

    #[test]
    fn sync_reports_not_a_git_worktree_for_non_git_dir() {
        let tmp = crate::test_support::hermetic_temp_dir();
        assert_eq!(
            sync_worktree_with_upstream(tmp.path()),
            SyncOutcome::NotAGitWorktree
        );
    }

    #[test]
    fn sync_fails_open_on_conflicting_uncommitted_change() {
        let Some(fx) = build_fixture() else {
            return;
        };
        let worktree = fx.worktree_dir.path();

        // Uncommitted edit to the SAME file the upstream commit changes —
        // git must refuse the fast-forward rather than clobber it.
        std::fs::write(worktree.join("CLAUDE.md"), "local uncommitted edit\n").unwrap();
        push_claude_md_update(&fx, "new content\n");

        let outcome = sync_worktree_with_upstream(worktree);
        assert!(
            matches!(outcome, SyncOutcome::Failed(_)),
            "expected a fail-open Failed outcome, got {outcome:?}"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("CLAUDE.md")).unwrap(),
            "local uncommitted edit\n",
            "a refused merge must leave the uncommitted local edit untouched"
        );
    }

    #[test]
    fn sync_fails_open_when_branches_have_diverged_with_local_commit() {
        // Why (code-critic review, #2647 item 5): the earlier conflict test
        // covers an UNCOMMITTED collision; this covers genuine branch
        // divergence — a real local COMMIT on the worktree branch that
        // origin/main does not have, while origin/main independently gains
        // an unrelated commit. Neither branch is an ancestor of the other,
        // so `git merge --ff-only` must refuse outright (a true merge or
        // rebase would be required, which this fail-open sync never
        // attempts), leaving the worktree's branch tip and working tree
        // completely untouched.
        let Some(fx) = build_fixture() else {
            return;
        };
        let worktree = fx.worktree_dir.path();

        // A genuine local commit on the worktree's own branch — e.g. the
        // operator committed work-in-progress before this resume.
        std::fs::write(worktree.join("OTHER.md"), "other v1 + local work\n").unwrap();
        assert!(git_ok(worktree, &["add", "-A"]));
        assert!(git_ok(worktree, &["commit", "-m", "local wip commit"]));
        let head_before = git_stdout(worktree, &["rev-parse", "HEAD"]).unwrap();

        // origin/main independently advances with an unrelated commit.
        push_claude_md_update(&fx, "new content\n");

        let outcome = sync_worktree_with_upstream(worktree);
        assert!(
            matches!(outcome, SyncOutcome::Failed(_)),
            "expected a fail-open Failed outcome on diverged histories, got {outcome:?}"
        );

        let head_after = git_stdout(worktree, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(
            head_before, head_after,
            "a refused non-fast-forward merge must never move the branch tip"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("OTHER.md")).unwrap(),
            "other v1 + local work\n",
            "the local commit's content must survive an untouched worktree"
        );
        assert_eq!(
            std::fs::read_to_string(worktree.join("CLAUDE.md")).unwrap(),
            "old content\n",
            "CLAUDE.md must stay at the worktree's own committed content, not \
             silently pick up origin's unrelated commit"
        );
    }

    #[test]
    fn self_heal_claude_md_strips_legacy_block() {
        let tmp = crate::test_support::hermetic_temp_dir();
        let begin = "<!-- trusty-mpm:delegation-directive:begin \
            (framework-owned — do not edit; regenerated on every session start, see issue #2125) -->";
        let end = "<!-- trusty-mpm:delegation-directive:end -->";
        let polluted = format!(
            "{begin}\n\nSome injected directive text.\n\n{end}\n\n# My Project\n\nNotes.\n"
        );
        std::fs::write(tmp.path().join("CLAUDE.md"), &polluted).unwrap();

        assert!(self_heal_claude_md(tmp.path()));

        let cleaned = std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap();
        assert!(!cleaned.contains(begin));
        assert!(!cleaned.contains("Some injected directive text."));
        assert_eq!(cleaned, "# My Project\n\nNotes.\n");
    }

    #[test]
    fn self_heal_claude_md_noop_when_clean() {
        let tmp = crate::test_support::hermetic_temp_dir();
        let clean = "# My Project\n\nNotes.\n";
        std::fs::write(tmp.path().join("CLAUDE.md"), clean).unwrap();

        assert!(!self_heal_claude_md(tmp.path()));
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("CLAUDE.md")).unwrap(),
            clean
        );
    }

    #[test]
    fn self_heal_claude_md_noop_when_absent() {
        let tmp = crate::test_support::hermetic_temp_dir();
        assert!(!self_heal_claude_md(tmp.path()));
    }
}
