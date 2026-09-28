//! Typed resume-error tests (regression guard for #1221 review findings, and
//! the #2577 WorkspaceGone/PaneGone split).
//!
//! Why: the resume HTTP handler previously chose its 404/409 status by
//! SUBSTRING-matching the error's `Display` string, which silently regressed to
//! 500 whenever wording changed; and it ran a pre-flight `get` before `resume`,
//! opening a TOCTOU window. These tests drive the shared `resume_managed` helper
//! directly and assert on the TYPED `ResumeManagedError` variant — never on the
//! Display string — so the 404/409/422 contract is enforced structurally.
//! Extracted from `session_manager_mvp.rs` (#2577 review) once the
//! `WorkspaceGone`/`PaneGone` split and its new `PaneGoneTmux` driver pushed
//! that file over its 1500-SLOC test cap — mirrors the existing
//! `resume_reattach_tests.rs` precedent of giving cap-pressured regression
//! coverage its own file.
//! What: the `ResumeManagedError::from(ManagedError)` mapping tests, plus the
//! four `resume_managed_typed_*` tests driving the full daemon-layer round
//! trip (state gate → workdir resolution → tmux liveness → error mapping) for
//! NotFound/InvalidState/WorkspaceGone/PaneGone.
//! Test: this file IS the test; run with `cargo test -p trusty-mpm`.

use std::sync::Arc;

use tempfile::TempDir;

use trusty_mpm::daemon::managed_routes::{ResumeManagedError, resume_managed};
use trusty_mpm::daemon::state::DaemonState;
use trusty_mpm::runtime::RuntimeKind as ResumeRuntimeKind;
use trusty_mpm::session_manager::{
    ManagedError, ManagedSessionId as ResumeSessionId, ManagedTmuxDriver,
};

/// #2577 (review-revised): `WorkspaceMissing` and `PaneGone` — both
/// operator-actionable on-disk preconditions — map to DISTINCT typed variants
/// (`WorkspaceGone`/`PaneGone`, both → HTTP 422), NOT the same shared variant
/// and NOT `Other` (→ 500). Each preserves the error's full actionable Display
/// message.
///
/// Why: the HTTP status is derived structurally from the variant, so pinning
/// the `From` mapping stops a future refactor from silently regressing either
/// condition back to a bare 500 — or, per the #2577 review, from silently
/// merging them back into one variant that cannot distinguish "safe to
/// delete" (`WorkspaceGone`) from "a live sibling window is at risk"
/// (`PaneGone`). `Other` is the catch-all for genuinely-internal faults and
/// must NOT swallow either.
/// What: converts each `ManagedError` variant and asserts both the resulting
/// `ResumeManagedError` variant and that the rendered string still names the
/// vanished path / pane id.
/// Test: this function IS the test.
#[test]
fn resume_error_workspace_missing_and_pane_gone_map_to_distinct_variants() {
    let ws_err = ResumeManagedError::from(ManagedError::WorkspaceMissing(
        "sess-1".into(),
        "/gone/worktree".into(),
    ));
    match ws_err {
        ResumeManagedError::WorkspaceGone(msg) => assert!(
            msg.contains("/gone/worktree"),
            "422 body must name the vanished workspace path, got {msg:?}"
        ),
        other => panic!("WorkspaceMissing must map to WorkspaceGone (→ 422), got {other:?}"),
    }

    let pane_err = ResumeManagedError::from(ManagedError::PaneGone("sess-2".into(), "%42".into()));
    match pane_err {
        ResumeManagedError::PaneGone(msg) => assert!(
            msg.contains("%42"),
            "422 body must name the vanished pane id, got {msg:?}"
        ),
        other => panic!("PaneGone must map to PaneGone (→ 422), got {other:?}"),
    }
}

/// A genuinely-internal manager fault (store I/O) must still map to `Other`
/// (→ HTTP 500) — the #2577 change narrowed the 422 path to exactly the two
/// on-disk-precondition variants and must not have widened it.
#[test]
fn resume_error_internal_fault_still_maps_to_other() {
    let io = ManagedError::Io(std::io::Error::other("disk gone"));
    assert!(
        matches!(ResumeManagedError::from(io), ResumeManagedError::Other(_)),
        "internal I/O faults must remain Other (→ 500)"
    );
}

/// Resuming a missing session yields the typed `NotFound` variant (→ HTTP 404).
///
/// Why: a session decommissioned (or never created) must produce a 404, and the
/// handler now derives that from the typed variant rather than a Display
/// substring. Removing the pre-flight `get` means this single round-trip is the
/// only place the 404 can originate — so we assert it lands here.
/// What: builds a fresh `DaemonState`, calls `resume_managed` with a random id,
/// and asserts the error is exactly `ResumeManagedError::NotFound`.
/// Test: this function IS the test.
#[tokio::test]
async fn resume_managed_typed_missing_session_is_not_found() {
    // Hermetic framework root with FakeNoopTmuxDriver so the test never touches
    // the operator's real `~/.trusty-mpm` or spawns real tmux sessions (#1790).
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let missing = ResumeSessionId::new();

    let err = resume_managed(&state, &missing)
        .await
        .expect_err("resuming a missing session must error");

    assert!(
        matches!(err, ResumeManagedError::NotFound(_)),
        "missing session must map to the typed NotFound variant (→ 404), got {err:?}"
    );
}

/// Resuming a non-resumable session yields the typed `InvalidState` variant
/// (→ HTTP 409) — driven WITHOUT depending on the Display string.
///
/// Why: only `Stopped`/`Errored` sessions are resumable. A freshly created
/// session is `Provisioning`, so resuming it is an invalid state transition that
/// must surface as a 409. The handler derives 409 from the typed variant; this
/// test proves the variant is produced for a non-resumable state.
/// What: seeds a session via the daemon's session manager (it starts in
/// `Provisioning`), calls `resume_managed`, and asserts the error matches
/// `ResumeManagedError::InvalidState`.
/// Test: this function IS the test.
#[tokio::test]
async fn resume_managed_typed_invalid_state_is_conflict() {
    // Hermetic framework root with FakeNoopTmuxDriver — no real tmux sessions
    // are created, so nothing can escape into the production store (#1790).
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;

    // A newly created record is in `Provisioning` — not `Stopped`/`Errored` — so
    // resuming it is an invalid state transition (→ 409), never a 404.
    let id = ResumeSessionId::new();
    // Derive a UNIQUE workspace path from the id: the tmux name is derived from
    // the cwd basename (and truncated to 20 chars), so a fixed path would collide
    // with a leftover tmux session (or a parallel run). Putting the UUID FIRST
    // keeps the name unique even after truncation; rooting it under the hermetic
    // temp dir keeps it isolated.
    let ws = root.path().join(format!("{id}-resume-ws"));
    let _seeded = mgr
        .create_with_id(
            id,
            "regression: invalid-state resume".to_string(),
            Some(ws.clone()),
            None,
            Some(ws),
            Some("https://example.com/r.git".to_string()),
            Some("main".to_string()),
            ResumeRuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session");

    // No real tmux session was created (FakeNoopTmuxDriver), so no reap guard is
    // needed (#1790). The seeded record exists only in the in-memory store.

    let err = resume_managed(&state, &id)
        .await
        .expect_err("resuming a Provisioning session must error");

    assert!(
        matches!(err, ResumeManagedError::InvalidState(_)),
        "non-resumable state must map to the typed InvalidState variant (→ 409), got {err:?}"
    );
}

/// #2577 regression: resuming a Stopped/Errored session whose workspace
/// directory no longer exists on disk yields the typed `WorkspaceGone` variant
/// (→ HTTP 422), NOT `Other` (→ HTTP 500) and NOT `PaneGone` (a different
/// remedy — see the #2577 review split documented on `ResumeManagedError`).
///
/// Why: an adopted/external session whose worktree was pruned (or any session
/// whose `last_cwd`/`workspace_path`/`cwd` all vanished) reaches
/// `resolve_existing_workdir`, which returns `ManagedError::WorkspaceMissing`
/// rather than handing a removed path to tmux. Before #2577 that mapped through
/// `Other` → 500, so the CLI printed a bare "daemon returned an internal error
/// (500)" with no hint that the worktree had simply been removed. The handler
/// now derives 422 from the typed `WorkspaceGone` variant; this test proves the
/// variant is produced (structurally, never via the Display string) so the
/// operator-actionable 422 contract cannot silently regress to 500 — or
/// silently collapse into the differently-remedied `PaneGone` variant.
/// What: seeds a session whose `cwd`/`workspace_path` point at a path that is
/// NEVER created on disk, drives it into `Errored` (a resumable state) via
/// `mark_errored`, calls `resume_managed`, and asserts the error matches
/// `ResumeManagedError::WorkspaceGone`.
/// Test: this function IS the test.
#[tokio::test]
async fn resume_managed_typed_missing_workspace_is_unprocessable() {
    // Hermetic framework root with FakeNoopTmuxDriver — no real tmux sessions
    // are created, so nothing escapes into the production store (#1790).
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;

    // A workspace path UNDER the temp root that is never actually created on
    // disk — every resume workdir candidate (`last_cwd` is None on a fresh
    // record, `workspace_path` and `cwd` are this vanished path) fails its
    // existence check, so `resolve_existing_workdir` returns `WorkspaceMissing`.
    // UUID-first keeps the derived tmux name unique after truncation (see the
    // invalid-state test above for the rationale).
    let id = ResumeSessionId::new();
    let gone = root.path().join(format!("{id}-removed-worktree"));
    let _seeded = mgr
        .create_with_id(
            id,
            "regression: missing-workspace resume".to_string(),
            Some(gone.clone()),
            None,
            Some(gone.clone()),
            Some("https://example.com/r.git".to_string()),
            Some("main".to_string()),
            ResumeRuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session");

    // Drive it into `Errored` (a resumable state) so `resume` passes the state
    // gate and reaches the workdir-resolution step where the missing directory
    // is detected. `mark_errored` sets `Errored` unconditionally.
    mgr.mark_errored(&id, "regression: simulate prior spawn failure")
        .await
        .expect("mark errored");

    // Guard against a stray real directory at the vanished path.
    assert!(
        !gone.exists(),
        "test precondition: the workspace path must NOT exist on disk"
    );

    let err = resume_managed(&state, &id)
        .await
        .expect_err("resuming a session whose workspace is gone must error");

    assert!(
        matches!(err, ResumeManagedError::WorkspaceGone(_)),
        "a removed workspace must map to the typed WorkspaceGone variant (→ 422), got {err:?}"
    );
}

/// #2577 review (MEDIUM finding 3a): drives `ManagedError::PaneGone` through
/// the REAL `resume_managed` daemon-layer function — not just the `From`
/// conversion in isolation
/// (`resume_error_workspace_missing_and_pane_gone_map_to_distinct_variants`
/// above) — proving the full round trip (state gate → workdir resolution →
/// `session_exists` → `ensure_pane_alive` → error mapping) actually reaches
/// `ResumeManagedError::PaneGone`.
///
/// Why: `resume_managed` is what BOTH the HTTP handler and the MCP
/// `session_resume` tool call; a unit test on the `From` impl alone cannot
/// prove the manager-level `PaneGone` (the #2467/#2468 sibling-window-hijack
/// guard — fires when the tmux SESSION is still alive via a sibling window but
/// the record's specific recorded pane is confirmed gone) actually reaches the
/// daemon's typed error at all.
/// What: seeds a session in a REAL (existing) temp workspace via a custom
/// driver (`PaneGoneTmux`) whose `create_session` records the tmux session as
/// "live" (so `session_exists` reports true) and whose `get_pane_id` returns a
/// fixed id (reproducing what a real spawn captures) while `pane_exists`
/// always reports `false` (the recorded pane is gone). Drives the record to
/// `Errored` (a resumable state) without touching tmux, then calls
/// `resume_managed` and asserts the typed `ResumeManagedError::PaneGone`
/// variant.
/// Test: this function IS the test.
#[tokio::test]
async fn resume_managed_typed_pane_gone_is_unprocessable() {
    let root = TempDir::new().unwrap();
    let workspace_dir = TempDir::new().unwrap();
    let driver = PaneGoneTmux::new("%42");
    let state = Arc::new(
        DaemonState::with_root_isolated_managed_and_driver(root.path().to_path_buf(), driver).await,
    );
    let mgr = state.session_manager().await;

    let ws = workspace_dir.path().to_owned();
    let record = mgr
        .create(
            "regression: pane-gone resume".to_string(),
            Some(ws.clone()),
            Some("pane-gone-session".into()),
            Some(ws),
            Some("https://example.com/r.git".to_string()),
            Some("main".to_string()),
        )
        .await
        .expect("create session");

    assert_eq!(
        record.pane_id.as_deref(),
        Some("%42"),
        "sanity: create() must have captured the driver's seeded pane_id"
    );

    // Drive to Errored (a resumable state) WITHOUT touching tmux, so the
    // driver's "live" bookkeeping from create_session is left untouched.
    mgr.mark_errored(&record.id, "regression: simulate prior spawn failure")
        .await
        .expect("mark errored");

    let err = resume_managed(&state, &record.id)
        .await
        .expect_err("resuming with a confirmed-gone recorded pane must error");

    assert!(
        matches!(err, ResumeManagedError::PaneGone(_)),
        "a confirmed-gone recorded pane (sibling window alive) must map to the \
         typed PaneGone variant (→ 422), got {err:?}"
    );
}

/// A tmux driver that tracks live sessions (mirroring `session_manager_mvp.rs`'s
/// `LiveTrackingTmux`) AND lets a test configure `get_pane_id`/`pane_exists`
/// independently — needed to drive the sibling-window-hijack path
/// (`ManagedError::PaneGone`) through the REAL `resume_managed` daemon-layer
/// function (#2577 review).
///
/// Why: the tmux SESSION must report alive (`session_exists` true, driving
/// `resume`'s reuse branch) while the recorded PANE reports gone
/// (`pane_exists` false) — a combination neither `FakeNoopTmuxDriver` (never
/// reports anything alive) nor `LiveTrackingTmux` (uses the trait's `true`
/// default for `pane_exists`) can express.
/// What: a `Mutex<HashSet<String>>` of live session names (mirroring
/// `LiveTrackingTmux`) plus a fixed `pane_id` returned by `get_pane_id`;
/// `pane_exists` unconditionally reports `false`, simulating the original pane
/// having closed while a sibling window keeps the session alive.
/// Test: `resume_managed_typed_pane_gone_is_unprocessable`.
struct PaneGoneTmux {
    live: std::sync::Mutex<std::collections::HashSet<String>>,
    pane_id: &'static str,
}

impl PaneGoneTmux {
    fn new(pane_id: &'static str) -> Arc<Self> {
        Arc::new(Self {
            live: std::sync::Mutex::new(std::collections::HashSet::new()),
            pane_id,
        })
    }
}

impl ManagedTmuxDriver for PaneGoneTmux {
    fn create_session(&self, name: &str, _workdir: &str) -> Result<(), ManagedError> {
        self.live.lock().unwrap().insert(name.to_owned());
        Ok(())
    }
    fn kill_session(&self, name: &str) -> Result<(), ManagedError> {
        self.live.lock().unwrap().remove(name);
        Ok(())
    }
    fn send_line(&self, _name: &str, _text: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(self.live.lock().unwrap().iter().cloned().collect())
    }
    fn get_pane_id(&self, _name: &str) -> Option<String> {
        Some(self.pane_id.to_string())
    }
    fn pane_exists(&self, _name: &str, _pane_id: &str) -> bool {
        false
    }
}
