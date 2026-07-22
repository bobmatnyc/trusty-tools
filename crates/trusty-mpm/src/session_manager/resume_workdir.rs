//! Workdir resolution + pane-cwd verification for `SessionManager::resume` (#2250).
//!
//! Why: `manager.rs` sits at the 500-SLOC production cap (same constraint that
//! forced `reactivate.rs`/`hook_sync.rs`/`create.rs` out into sibling files),
//! so the two new guards `resume()` needs land here instead of growing that
//! file further. Prior to #2250, `resume()`'s recreate branch picked
//! `workspace_path` as a fallback WITHOUT an existence check (unlike
//! `last_cwd`, which already had one), and a `create_session` success was
//! trusted blindly. `tmux new-session -c <missing-dir>` exits 0 but silently
//! roots the pane at `$HOME` — the project-tier `.claude/` skills/persona/MCP
//! config that only exists under the real workspace is silently lost the
//! instant that happens, because `claude --setting-sources project,local`
//! only reads `<cwd>/.claude`.
//! What: [`resolve_existing_workdir`] picks the first of
//! (`last_cwd`, `workspace_path`, `cwd`) that exists on disk, returning a typed
//! [`ManagedError::WorkspaceMissing`] when none do (never a silent `$HOME`
//! fallback); [`verify_pane_cwd`] compares the driver-reported pane cwd against
//! the expected workdir right after a fresh `create_session`, failing loudly on
//! a mismatch (tmux silently fell back) rather than proceeding to type the
//! resume command into a mis-rooted pane. A driver that cannot report the pane
//! cwd (`get_pane_cwd` returns `None` — the trait default, and every test
//! double unless it opts in) is treated as "cannot verify", not a mismatch.
//! Test: `resolve_existing_workdir_prefers_first_existing`,
//! `resolve_existing_workdir_falls_back_through_candidates`,
//! `resolve_existing_workdir_errors_when_all_missing`,
//! `verify_pane_cwd_ok_on_match`, `verify_pane_cwd_ok_when_unknown`,
//! `verify_pane_cwd_errors_on_mismatch`, `is_unresumable_*` (this file's
//! `#[cfg(test)]` module); end-to-end coverage of the recreate branch lives in
//! `resume_reattach_tests.rs`.

use std::path::{Path, PathBuf};

use super::ManagedTmuxDriver;
use super::manager::ManagedError;
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};

/// The three resume-workdir candidates for `record`, in priority order.
///
/// Why: [`resolve_existing_workdir`] and [`is_unresumable`] (#2595) must probe
/// the EXACT same candidates in the EXACT same order — a divergence here would
/// mean a session `resolve_existing_workdir` can still resume gets flagged
/// dead (or vice versa). Factoring the candidate list out of both callers
/// makes that impossible.
/// What: `[last_cwd, workspace_path, cwd]`, `None`-filtered by the caller via
/// `.into_iter().flatten()`.
/// Test: exercised transitively by every `resolve_existing_workdir_*` and
/// `is_unresumable_*` test.
fn workdir_candidates(record: &SessionRecord) -> [Option<&Path>; 3] {
    [
        record.last_cwd.as_deref(),
        record.workspace_path.as_deref(),
        Some(record.cwd.as_path()),
    ]
}

/// Pick the first candidate workdir that exists on disk, in priority order.
///
/// Why: see module doc — `resume()` must never hand a stale/removed path to
/// tmux; all three fallback candidates need the same existence guard
/// `last_cwd` already had before #2250 (`workspace_path` and `cwd` did not).
/// Takes `&SessionRecord` (rather than three separate `Option<&Path>`
/// parameters) purely to keep the call site in `manager.rs` — already at its
/// 500-SLOC cap — a single short line.
/// What: checks `record.last_cwd`, then `record.workspace_path`, then
/// `record.cwd` in order (via [`workdir_candidates`]) using
/// `tokio::fs::try_exists` (non-blocking); returns the first that exists.
/// When none do, returns [`ManagedError::WorkspaceMissing`] carrying `id` and
/// the most-informative candidate (`workspace_path` if set, else `cwd`) so the
/// operator knows exactly which directory vanished.
/// Test: `resolve_existing_workdir_prefers_first_existing`,
/// `resolve_existing_workdir_falls_back_through_candidates`,
/// `resolve_existing_workdir_errors_when_all_missing`.
pub(super) async fn resolve_existing_workdir(
    id: &ManagedSessionId,
    record: &SessionRecord,
) -> Result<PathBuf, ManagedError> {
    for candidate in workdir_candidates(record).into_iter().flatten() {
        if tokio::fs::try_exists(candidate).await.unwrap_or(false) {
            return Ok(candidate.to_path_buf());
        }
    }
    let missing = record.workspace_path.as_deref().unwrap_or(&record.cwd);
    Err(ManagedError::WorkspaceMissing(
        id.to_string(),
        missing.display().to_string(),
    ))
}

/// The shared "unresumable" predicate (#2595): true when a stopped/errored
/// session has NO workdir candidate left on disk, meaning any resume attempt
/// is guaranteed to fail with [`ManagedError::WorkspaceMissing`].
///
/// Why (#2595, builds on #2594's `ResumeManagedError::Unresumable` semantics):
/// before this, an operator-facing session picker offered "restart" for a
/// session whose workspace had been GC-pruned — the restart could only ever
/// 422. #2594 fixed the ERROR response; this predicate lets every LISTING
/// surface (the bare-`tm` guided default, `tm ls`, `tm sessions ls` — all of
/// which read `GET /api/v1/sessions/managed`) exclude/flag the session
/// BEFORE the operator ever selects it, instead of only failing loudly after
/// the fact. Gated to `Stopped`/`Errored` (mirrors the CLI's own
/// `guided_resume::needs_restart`) because a live/provisioning session has (or
/// is about to have) a real tmux pane — whether its ORIGINAL workdir still
/// exists is irrelevant to whether it can be resumed (reattached).
/// What: returns `false` immediately for any state other than
/// `Stopped`/`Errored` (no I/O). For `Stopped`/`Errored`, probes the SAME
/// three candidates [`resolve_existing_workdir`] does (via
/// [`workdir_candidates`]) with `tokio::fs::try_exists`; returns `true` only
/// when every candidate probe comes back `Ok(false)` (definitively absent).
///
/// **Deliberately diverges from [`resolve_existing_workdir`] on a probe
/// `Err`** (PR #2652 review, MEDIUM finding 3): this predicate is FAIL-OPEN —
/// a transient I/O/permission/network-volume error on any candidate is
/// treated as "possibly present" and short-circuits to `false` (never
/// flagged dead), the same as `Ok(true)`. `resolve_existing_workdir` keeps
/// its existing `Err → treat as absent` behavior (`.unwrap_or(false)`)
/// UNCHANGED — the two callers have opposite failure trade-offs by design:
/// `resolve_existing_workdir` guards an ACTUAL resume attempt, where refusing
/// to hand tmux a possibly-bad path on an unprobable candidate is the SAFE
/// failure mode (#2250's original "never silently fall back" intent), and the
/// #2594 422 the operator sees is retryable. This predicate instead gates
/// whether a session is even OFFERED in a picker — wrongly flagging it dead
/// on a transient probe error leaves the operator with only a destructive
/// delete as the way out, which is the wrong trade-off for a condition that
/// may clear on the next poll. `workdir_candidates` (the candidate LIST) is
/// still shared so the two can never disagree about WHICH paths to check —
/// only how an inconclusive probe on one of them is interpreted.
/// Test: `is_unresumable_true_when_all_candidates_missing_and_stopped`,
/// `is_unresumable_false_when_any_candidate_exists`,
/// `is_unresumable_false_for_non_stopped_states_even_when_all_missing`,
/// `is_unresumable_fails_open_on_probe_error`.
pub(crate) async fn is_unresumable(record: &SessionRecord) -> bool {
    if !matches!(
        record.state,
        ManagedSessionState::Stopped | ManagedSessionState::Errored
    ) {
        return false;
    }
    for candidate in workdir_candidates(record).into_iter().flatten() {
        match tokio::fs::try_exists(candidate).await {
            // Definitively present, or the probe itself couldn't tell
            // (permission denied, transient I/O, an unmounted network
            // volume, …) — either way, do NOT flag this session dead.
            Ok(true) | Err(_) => return false,
            Ok(false) => continue,
        }
    }
    true
}

/// Verify a freshly created tmux pane actually landed at `expected`.
///
/// Why: `tmux new-session -c <missing-dir>` exits 0 but silently roots the
/// pane at `$HOME` — exit status alone can never detect this (#2250); this is
/// the only point where the ACTUAL pane cwd can be cross-checked before the
/// resume command is typed into it.
/// What: reads `driver.get_pane_cwd(name)`; `None` (driver doesn't support the
/// probe — the trait default, and every test double unless it opts in) is
/// treated as "cannot verify", not a failure, so drivers/tests that never
/// implement it don't spuriously break. `Some(actual)` is compared against
/// `expected`, canonicalizing both when possible (falls back to the raw path
/// when canonicalization fails, e.g. a path that doesn't exist from this
/// process's perspective) so a symlinked tmp dir does not spuriously
/// mismatch; a real mismatch returns [`ManagedError::TmuxUnavailable`] with an
/// actionable message instead of silently proceeding.
/// Test: `verify_pane_cwd_ok_on_match`, `verify_pane_cwd_ok_when_unknown`,
/// `verify_pane_cwd_errors_on_mismatch`.
pub(super) fn verify_pane_cwd(
    driver: &dyn ManagedTmuxDriver,
    name: &str,
    expected: &Path,
) -> Result<(), ManagedError> {
    let Some(actual) = driver.get_pane_cwd(name) else {
        return Ok(());
    };
    let expected_c = expected
        .canonicalize()
        .unwrap_or_else(|_| expected.to_path_buf());
    let actual_c = actual.canonicalize().unwrap_or_else(|_| actual.clone());
    if expected_c == actual_c {
        Ok(())
    } else {
        Err(ManagedError::TmuxUnavailable(format!(
            "tmux session '{name}' pane landed at '{}' instead of the requested workspace \
             '{}' — tmux silently fell back (likely $HOME) because the target directory was \
             unusable; refusing to type the resume command into a mis-rooted pane",
            actual.display(),
            expected.display()
        )))
    }
}

/// Create a fresh tmux session at `workdir`, then verify it actually landed
/// there (#2250).
///
/// Why: `manager.rs::resume`'s recreate branch needs both steps back-to-back
/// every time it creates a session — folding them into one call keeps that
/// call site a single line instead of growing `manager.rs` (already at its
/// 500-SLOC cap) by the width of both.
/// What: `driver.create_session(name, workdir)` (mapped to
/// [`ManagedError::TmuxUnavailable`] on failure), then
/// [`verify_pane_cwd`] against the same `workdir`.
/// Test: covered end-to-end via `SessionManager::resume` in
/// `resume_reattach_tests.rs` (`manager_resume_respawns_in_existing_workspace`,
/// `manager_resume_errors_when_recreated_pane_cwd_mismatches`).
pub(super) fn create_and_verify_pane(
    driver: &dyn ManagedTmuxDriver,
    name: &str,
    workdir: &str,
) -> Result<(), ManagedError> {
    driver
        .create_session(name, workdir)
        .map_err(|e| ManagedError::TmuxUnavailable(e.to_string()))?;
    verify_pane_cwd(driver, name, Path::new(workdir))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;

    use chrono::Utc;
    use tempfile::TempDir;

    use super::super::record::ManagedSessionState;
    use super::*;

    /// Minimal `SessionRecord` builder for `resolve_existing_workdir` tests —
    /// only `cwd`/`workspace_path`/`last_cwd` vary between cases.
    fn make_record(
        cwd: PathBuf,
        workspace_path: Option<PathBuf>,
        last_cwd: Option<PathBuf>,
    ) -> SessionRecord {
        SessionRecord {
            id: ManagedSessionId::new(),
            tmux_name: "tmpm-resume-workdir-test".into(),
            cwd,
            task: "test".into(),
            state: ManagedSessionState::Stopped,
            created_at: Utc::now(),
            last_activity_at: None,
            workspace_path,
            repo_url: None,
            branch: None,
            pending_decision: None,
            proposed_default: None,
            correlation: Default::default(),
            runtime: Default::default(),
            ephemeral: false,
            workspace_owned: false,
            source_id: None,
            claude_session_id: None,
            scrollback_path: None,
            last_cwd,
            deliverable_id: None,
            pane_id: None,
            injection_status: Default::default(),
            worktree_owner: None,
        }
    }

    #[tokio::test]
    async fn resolve_existing_workdir_prefers_first_existing() {
        let last = TempDir::new().unwrap();
        let workspace = TempDir::new().unwrap();
        let id = ManagedSessionId::new();
        let record = make_record(
            last.path().to_owned(),
            Some(workspace.path().to_owned()),
            Some(last.path().to_owned()),
        );
        let resolved = resolve_existing_workdir(&id, &record)
            .await
            .expect("last_cwd exists, must be chosen first");
        assert_eq!(resolved, last.path());
    }

    #[tokio::test]
    async fn resolve_existing_workdir_falls_back_through_candidates() {
        let workspace = TempDir::new().unwrap();
        let cwd = TempDir::new().unwrap();
        let id = ManagedSessionId::new();
        let missing_last = PathBuf::from("/nonexistent/last-cwd-2250");

        // last_cwd missing -> falls back to workspace_path.
        let record = make_record(
            cwd.path().to_owned(),
            Some(workspace.path().to_owned()),
            Some(missing_last.clone()),
        );
        let resolved = resolve_existing_workdir(&id, &record)
            .await
            .expect("workspace_path exists, must be chosen over missing last_cwd");
        assert_eq!(resolved, workspace.path());

        // last_cwd AND workspace_path missing -> falls back to cwd.
        let missing_workspace = PathBuf::from("/nonexistent/workspace-2250");
        let record = make_record(
            cwd.path().to_owned(),
            Some(missing_workspace),
            Some(missing_last),
        );
        let resolved = resolve_existing_workdir(&id, &record)
            .await
            .expect("cwd exists, must be the final fallback");
        assert_eq!(resolved, cwd.path());
    }

    #[tokio::test]
    async fn resolve_existing_workdir_errors_when_all_missing() {
        let id = ManagedSessionId::new();
        let missing_workspace = PathBuf::from("/nonexistent/workspace-2250");
        let record = make_record(
            PathBuf::from("/nonexistent/cwd-2250"),
            Some(missing_workspace.clone()),
            Some(PathBuf::from("/nonexistent/last-cwd-2250")),
        );

        let err = resolve_existing_workdir(&id, &record).await.expect_err(
            "no candidate exists on disk — must error, not silently fall back to $HOME",
        );

        match err {
            ManagedError::WorkspaceMissing(err_id, path) => {
                assert_eq!(err_id, id.to_string());
                assert_eq!(path, missing_workspace.display().to_string());
            }
            other => panic!("expected WorkspaceMissing, got {other:?}"),
        }
    }

    // ── is_unresumable (#2595) ───────────────────────────────────────────

    #[tokio::test]
    async fn is_unresumable_true_when_all_candidates_missing_and_stopped() {
        let mut record = make_record(
            PathBuf::from("/nonexistent/cwd-2595"),
            Some(PathBuf::from("/nonexistent/workspace-2595")),
            Some(PathBuf::from("/nonexistent/last-cwd-2595")),
        );
        record.state = ManagedSessionState::Stopped;
        assert!(
            is_unresumable(&record).await,
            "stopped session with no existing candidate must be unresumable"
        );

        // Errored is the sibling resumable-from state and must behave identically.
        record.state = ManagedSessionState::Errored;
        assert!(
            is_unresumable(&record).await,
            "errored session with no existing candidate must be unresumable"
        );
    }

    #[tokio::test]
    async fn is_unresumable_false_when_any_candidate_exists() {
        let cwd = TempDir::new().unwrap();
        // Only `cwd` (the last-resort candidate) exists on disk; last_cwd and
        // workspace_path are both gone — this must still be resumable, exactly
        // mirroring `resolve_existing_workdir`'s fallback-to-cwd behavior.
        let mut record = make_record(
            cwd.path().to_owned(),
            Some(PathBuf::from("/nonexistent/workspace-2595")),
            Some(PathBuf::from("/nonexistent/last-cwd-2595")),
        );
        record.state = ManagedSessionState::Stopped;
        assert!(
            !is_unresumable(&record).await,
            "a session with ANY existing candidate must not be flagged unresumable"
        );
    }

    #[tokio::test]
    async fn is_unresumable_false_for_non_stopped_states_even_when_all_missing() {
        // Why: a live/provisioning session already has (or is about to have) a
        // real tmux pane; whether its original workdir still exists on disk is
        // irrelevant to whether it can be resumed (reattached). The predicate
        // must short-circuit to `false` WITHOUT probing the filesystem.
        let mut record = make_record(
            PathBuf::from("/nonexistent/cwd-2595"),
            Some(PathBuf::from("/nonexistent/workspace-2595")),
            Some(PathBuf::from("/nonexistent/last-cwd-2595")),
        );
        for state in [
            ManagedSessionState::Active,
            ManagedSessionState::Provisioning,
        ] {
            record.state = state;
            assert!(
                !is_unresumable(&record).await,
                "state {:?} must never be flagged unresumable regardless of workdir",
                record.state
            );
        }
    }

    /// #2595 (PR #2652 review, MEDIUM finding 3): a probe `Err` on a
    /// candidate — NOT "not found", a genuine I/O failure — must fail OPEN
    /// (never flag the session dead), the opposite of `resolve_existing_workdir`'s
    /// `Err → treat as absent` behavior (see [`is_unresumable`]'s doc for why
    /// the two callers deliberately diverge here).
    #[tokio::test]
    async fn is_unresumable_fails_open_on_probe_error() {
        // A regular FILE used as a directory component makes any stat of a
        // path beneath it fail with ENOTDIR — a genuine I/O error, not
        // "not found" — which is a reliable, cross-platform way to force
        // `tokio::fs::try_exists` into its `Err` branch rather than `Ok(false)`.
        let dir = TempDir::new().unwrap();
        let not_a_dir = dir.path().join("this-is-a-file-not-a-dir");
        std::fs::write(&not_a_dir, b"x").expect("write stand-in file");
        let unprobable = not_a_dir.join("child-cannot-be-stat-ed");

        // Placed as `last_cwd` — the FIRST candidate checked — so the
        // fail-open short-circuit fires before `workspace_path`/`cwd` (both
        // definitely-missing) are ever reached.
        let mut record = make_record(
            PathBuf::from("/nonexistent/cwd-2595-failopen"),
            Some(PathBuf::from("/nonexistent/workspace-2595-failopen")),
            Some(unprobable),
        );
        record.state = ManagedSessionState::Stopped;

        assert!(
            !is_unresumable(&record).await,
            "a probe error on any candidate must fail OPEN (never flag dead) — a \
             transient I/O/permission/network-volume error is not proof the \
             workspace is gone, and the picker's only override would otherwise \
             be a destructive delete"
        );
    }

    /// Minimal `ManagedTmuxDriver` test double whose `get_pane_cwd` is
    /// controllable, so `verify_pane_cwd` can be exercised without pulling in
    /// the full `FakeTmuxDriver` from `tests.rs`.
    struct StubDriver {
        pane_cwd: Mutex<Option<PathBuf>>,
    }

    impl ManagedTmuxDriver for StubDriver {
        fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
            Ok(())
        }
        fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
            Ok(String::new())
        }
        fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
            Ok(Vec::new())
        }
        fn get_pane_cwd(&self, _name: &str) -> Option<PathBuf> {
            self.pane_cwd.lock().unwrap().clone()
        }
    }

    #[test]
    fn verify_pane_cwd_ok_on_match() {
        let dir = TempDir::new().unwrap();
        let driver = StubDriver {
            pane_cwd: Mutex::new(Some(dir.path().to_path_buf())),
        };
        assert!(verify_pane_cwd(&driver, "s", dir.path()).is_ok());
    }

    #[test]
    fn verify_pane_cwd_ok_when_unknown() {
        // Driver reports no pane cwd (the trait default) — "cannot verify" is
        // NOT the same as a mismatch, so this must succeed.
        let dir = TempDir::new().unwrap();
        let driver = StubDriver {
            pane_cwd: Mutex::new(None),
        };
        assert!(verify_pane_cwd(&driver, "s", dir.path()).is_ok());
    }

    #[test]
    fn verify_pane_cwd_errors_on_mismatch() {
        let expected = TempDir::new().unwrap();
        let actual = TempDir::new().unwrap();
        let driver = StubDriver {
            pane_cwd: Mutex::new(Some(actual.path().to_path_buf())),
        };
        let err = verify_pane_cwd(&driver, "s", expected.path())
            .expect_err("pane cwd disagrees with the requested workdir — must fail loudly");
        assert!(matches!(err, ManagedError::TmuxUnavailable(_)));
    }
}
