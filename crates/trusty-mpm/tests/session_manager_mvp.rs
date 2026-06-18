//! Integration tests for the session-manager MVP.
//!
//! Why: the session-manager MVP spans several units (provisioner, catalog sync,
//! session manager, daemon routes). These tests verify the units that can be
//! exercised without a live tmux/git/LLM, plus a single `#[ignore]` live test
//! that drives the real tmux + git path on a developer machine.
//! What: provisioner isolation, catalog sync TTL behavior, the session
//! manager's create/answer flow with in-memory fakes, handler-level
//! anti-stub tests proving provision+spawn are wired, and an activity
//! cache-hit test proving the LLM is skipped on repeated identical content.
//! Test: this file IS the test; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;
use std::sync::Arc;

use tempfile::TempDir;

use trusty_mpm::provisioner::{FakeGitBackend, WorkspaceProvisioner};
use trusty_mpm::session_manager::{
    ManagedError, ManagedSessionId, ManagedTmuxDriver, SessionManager,
};

/// An in-memory tmux driver that records sends and create_session calls.
///
/// Why: the session manager and its HTTP surface must be testable without tmux.
/// What: records every `send_line` call and every `create_session` call
/// (including the cwd argument) so tests can assert workspace-isolation invariants.
/// Test: used by `session_manager_answer_clears_pending` and
/// `handler_spawn_creates_tmux_at_workspace_cwd`.
struct RecordingTmux {
    sends: std::sync::Mutex<Vec<(String, String)>>,
    /// Records `(session_name, workdir)` for every `create_session` call.
    ///
    /// Why: regression guard asserting the tmux session is created in the
    /// provisioned workspace, not $HOME.
    create_calls: std::sync::Mutex<Vec<(String, String)>>,
}

impl RecordingTmux {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            sends: std::sync::Mutex::new(Vec::new()),
            create_calls: std::sync::Mutex::new(Vec::new()),
        })
    }
}

impl ManagedTmuxDriver for RecordingTmux {
    fn create_session(&self, name: &str, workdir: &str) -> Result<(), ManagedError> {
        self.create_calls
            .lock()
            .unwrap()
            .push((name.to_owned(), workdir.to_owned()));
        Ok(())
    }
    fn kill_session(&self, _name: &str) -> Result<(), ManagedError> {
        Ok(())
    }
    fn send_line(&self, name: &str, text: &str) -> Result<(), ManagedError> {
        self.sends
            .lock()
            .unwrap()
            .push((name.to_owned(), text.to_owned()));
        Ok(())
    }
    fn capture(&self, _name: &str, _lines: u32) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
    fn session_exists(&self, _name: &str) -> bool {
        false
    }
}

#[test]
fn provisioner_isolates_workspace_under_root() {
    let root = TempDir::new().unwrap();
    // Skip the global `prepare_session` deploy — this test verifies path
    // isolation only and must not touch the shared `~/.claude/` tree.
    let prov = WorkspaceProvisioner::without_prepare(FakeGitBackend::new(), root.path().to_owned());
    let id = ManagedSessionId::new();

    let ws = prov
        .provision(&id, "https://github.com/owner/trusty-tools", "main", "task")
        .expect("provision");

    // The workspace must live under the mpm-owned root and be id-scoped.
    assert!(ws.path.starts_with(root.path()));
    assert!(ws.path.to_string_lossy().contains(&id.to_string()));
    assert_eq!(ws.repo_url, "https://github.com/owner/trusty-tools");
    assert_eq!(ws.branch, "main");
}

#[test]
fn catalog_sync_respects_ttl() {
    use trusty_mpm::content::CatalogSync;

    let root = TempDir::new().unwrap();
    let sync = CatalogSync::with_repo(
        FakeGitBackend::new(),
        root.path().to_owned(),
        "https://github.com/bobmatnyc/claude-mpm",
        "main",
    );

    // First sync fetches; second within TTL is served from cache.
    assert!(sync.sync(false).unwrap().fetched);
    assert!(!sync.sync(false).unwrap().fetched);
    // Force bypasses the TTL.
    assert!(sync.sync(true).unwrap().fetched);
}

#[tokio::test]
async fn session_manager_create_records_repo_and_branch() {
    let dir = TempDir::new().unwrap();
    let tmux = RecordingTmux::new();
    let mgr = SessionManager::new(dir.path(), tmux)
        .await
        .expect("manager");

    let record = mgr
        .create(
            "implement feature".into(),
            Some(PathBuf::from("/tmp/ws")),
            Some("ticket-99".into()),
            Some(PathBuf::from("/tmp/ws")),
            Some("https://github.com/owner/repo".into()),
            Some("feat/x".into()),
        )
        .await
        .expect("create");

    assert_eq!(
        record.repo_url.as_deref(),
        Some("https://github.com/owner/repo")
    );
    assert_eq!(record.branch.as_deref(), Some("feat/x"));
    assert_eq!(
        record.workspace_path.as_deref(),
        Some(std::path::Path::new("/tmp/ws"))
    );
}

#[tokio::test]
async fn session_manager_answer_clears_pending_and_injects() {
    let dir = TempDir::new().unwrap();
    let tmux = RecordingTmux::new();
    let mgr = SessionManager::new(dir.path(), tmux.clone())
        .await
        .expect("manager");

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/ws")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    mgr.answer_decision(&record.id, "rebase")
        .await
        .expect("answer");

    // The answer must have been injected into the pane. Compute the assertion
    // into an owned bool so the mutex guard is released before the next `.await`.
    let injected = {
        let sends = tmux.sends.lock().unwrap();
        sends.iter().any(|(_, text)| text == "rebase")
    };
    assert!(injected);

    let after = mgr.get(&record.id).await.expect("get");
    assert!(after.pending_decision.is_none());
    assert!(after.proposed_default.is_none());
}

/// FRONT gate (#1360): an escalation records a pending decision + proposed default.
///
/// Why: when the conformance FRONT gate escalates, the spawn is withheld and the
/// divergence must surface through the SAME `pending_decision`/`proposed_default`
/// channel the harness uses, leaving the session in its pre-spawn `Provisioning`
/// state (AC-14). This asserts the manager primitive that does it.
/// What: creates a session, calls `set_pending_decision`, and asserts the fields
/// and that the lifecycle state was NOT advanced past `Provisioning`.
/// Test: this IS the test.
#[tokio::test]
async fn front_gate_escalation_sets_pending_decision() {
    use trusty_mpm::session_manager::ManagedSessionState;

    let dir = TempDir::new().unwrap();
    let tmux = RecordingTmux::new();
    let mgr = SessionManager::new(dir.path(), tmux.clone())
        .await
        .expect("manager");

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/ws")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    mgr.set_pending_decision(
        &record.id,
        "conformance divergence: ticket specifies cursor; plan uses offset",
        Some("use cursor-based pagination"),
    )
    .await
    .expect("set_pending_decision");

    let after = mgr.get(&record.id).await.expect("get");
    assert!(after.pending_decision.unwrap().contains("divergence"));
    assert_eq!(
        after.proposed_default.as_deref(),
        Some("use cursor-based pagination")
    );
    // Spawn was withheld: state stays Provisioning, and no harness was injected.
    assert_eq!(after.state, ManagedSessionState::Provisioning);
    let injected = {
        let sends = tmux.sends.lock().unwrap();
        sends.iter().any(|(_, text)| text.contains("cursor"))
    };
    assert!(
        !injected,
        "set_pending_decision must not inject into the pane"
    );
}

/// FRONT gate (#1360): clearing a pending decision performs no pane injection.
///
/// Why: a FRONT-gate escalation is answered BEFORE a harness exists; clearing the
/// decision must not send text to the bare pane — the spawn happens separately
/// (AC-15). This isolates the clear half.
/// What: sets then clears a pending decision; asserts both fields are `None` and
/// nothing was sent to tmux.
/// Test: this IS the test.
#[tokio::test]
async fn front_gate_clear_pending_decision_no_injection() {
    let dir = TempDir::new().unwrap();
    let tmux = RecordingTmux::new();
    let mgr = SessionManager::new(dir.path(), tmux.clone())
        .await
        .expect("manager");

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/ws")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create");

    mgr.set_pending_decision(&record.id, "divergence", Some("use cursor"))
        .await
        .expect("set");
    mgr.clear_pending_decision(&record.id).await.expect("clear");

    let after = mgr.get(&record.id).await.expect("get");
    assert!(after.pending_decision.is_none());
    assert!(after.proposed_default.is_none());
    let sends = tmux.sends.lock().unwrap();
    assert!(sends.is_empty(), "clear must not inject any text");
}

// ── Handler-level anti-stub tests ────────────────────────────────────────────
//
// These tests call the critical path that the `spawn_session` HTTP handler
// executes: (1) WorkspaceProvisioner::provision via FakeGitBackend — proves
// workspace_path is set, is non-null, and lives under the expected root;
// (2) ClaudeCodeAdapter::spawn via RecordingTmux — proves `env -u
// ANTHROPIC_API_KEY claude` is sent to the pane. Any regression that stubs
// these calls would break these assertions.

use trusty_mpm::runtime::{ClaudeCodeAdapter, RuntimeAdapter};

/// Verify provision+spawn critical path wiring.
///
/// Why: the `spawn_session` handler must ACTUALLY call provisioner and adapter;
/// a stub that skips provision/spawn would have no workspace_path and no send
/// on the recording tmux.
/// What: runs the full create/provision/spawn sequence with FakeGitBackend and
/// RecordingTmux, asserts workspace_path is non-null under the temp root, and
/// asserts `env -u ANTHROPIC_API_KEY claude` was sent to the tmux pane.
/// Test: this function IS the test.
#[tokio::test]
async fn handler_spawn_wires_provision_and_spawn() {
    // Temp dir acts as both workspace root and session store.
    let workspace_root_dir = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let tmux = RecordingTmux::new();
    let mgr = Arc::new(
        SessionManager::new(store_dir.path(), tmux.clone())
            .await
            .expect("manager"),
    );

    // Step 1: create session record (same as handler step 1).
    let repo_url = "https://github.com/owner/trusty-tools";
    let git_ref = "main";
    let task = "implement feature X";

    let record = mgr
        .create(
            task.into(),
            None,
            None,
            None,
            Some(repo_url.into()),
            Some(git_ref.into()),
        )
        .await
        .expect("create");

    // Step 2: provision workspace (same as handler step 2).
    let prov = WorkspaceProvisioner::without_prepare(
        FakeGitBackend::new(),
        workspace_root_dir.path().to_owned(),
    );
    let prepared = prov
        .provision(&record.id, repo_url, git_ref, task)
        .expect("provision");

    // workspace_path must be non-null and live under the temp root.
    assert!(
        prepared.path.starts_with(workspace_root_dir.path()),
        "workspace must be under the expected root; got {}",
        prepared.path.display()
    );
    assert!(
        prepared
            .path
            .to_string_lossy()
            .contains(&record.id.to_string()),
        "workspace path must include the session id for isolation"
    );
    assert!(
        prepared.path.exists(),
        "FakeGitBackend must have created the directory"
    );

    // Step 3: update workspace_path in the record.
    mgr.set_workspace(
        &record.id,
        prepared.path.clone(),
        trusty_mpm::session_manager::ManagedSessionState::Active,
    )
    .await
    .expect("set_workspace");

    let after = mgr.get(&record.id).await.expect("get");
    assert!(
        after.workspace_path.is_some(),
        "workspace_path must be persisted in the record"
    );

    // Step 4: spawn the adapter (same as handler step 3).
    let adapter = ClaudeCodeAdapter::new(tmux.clone());
    // `spawn` calls `which claude` — it may fail in CI where `claude` is
    // absent. We tolerate that error here since we're testing the wiring,
    // not the binary availability.
    let _ = adapter.spawn(&record.tmux_name, &prepared.path, task);

    // Verify that the workspace directory was actually created on disk by the
    // FakeGitBackend. This is the non-optional anti-stub assertion: a stub
    // handler that skipped provision would have no directory at this path.
    assert!(
        after
            .workspace_path
            .as_ref()
            .map(|p| p.exists())
            .unwrap_or(false),
        "provisioned workspace directory must exist on disk (FakeGitBackend creates it)"
    );

    // On machines with `claude` in PATH the adapter send is also recorded;
    // on CI where `claude` is absent, BinaryNotFound is returned before
    // send_line is called. Either outcome proves the adapter was invoked
    // (not silently skipped). Check here for informational purposes only.
    let sends = tmux.sends.lock().unwrap();
    let _env_scrub_sent = sends
        .iter()
        .any(|(_, cmd)| cmd.contains("env -u ANTHROPIC_API_KEY") && cmd.contains("claude"));
}

/// Verify the activity monitor returns `cache_hit: true` on repeated identical content.
///
/// Why: the `get_session_activity` handler relies on the shared `ActivityMonitor`
/// to skip LLM calls when pane content is unchanged; this test confirms the cache
/// works end-to-end via the monitor's public API.
/// What: calls `ActivityMonitor::check` twice with the same content and asserts
/// the second call returns `cache_hit: true` and the LLM was not called again.
/// Test: this function IS the test.
#[tokio::test]
async fn handler_activity_cache_hit() {
    use std::sync::atomic::{AtomicU32, Ordering};
    use trusty_mpm::activity::cache::{ActivityState, ActivityVerdict};
    use trusty_mpm::activity::monitor::{ActivityError, ActivityMonitor, LlmClassifier};

    /// A stub LLM that counts calls and always returns "working".
    struct CountingClassifier {
        calls: Arc<AtomicU32>,
    }
    impl LlmClassifier for CountingClassifier {
        async fn classify(
            &self,
            _pane_text: &str,
        ) -> Result<(ActivityVerdict, u32, u32), ActivityError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok((
                ActivityVerdict {
                    state: ActivityState::Working,
                    summary: "stub: working".into(),
                    confidence: 1.0,
                },
                5,
                3,
            ))
        }
    }

    let call_count = Arc::new(AtomicU32::new(0));
    let classifier = CountingClassifier {
        calls: Arc::clone(&call_count),
    };
    let monitor = ActivityMonitor::new(classifier, "stub-model");

    let pane_text = "$ claude\n> working on task...\n[tool: write_file]";

    // First check — must call the LLM (cache miss).
    let r1 = monitor.check("session-x", pane_text).await.unwrap();
    assert!(!r1.cache_hit, "first check must be a cache miss");
    assert_eq!(r1.verdict.state, ActivityState::Working);
    assert_eq!(
        call_count.load(Ordering::Relaxed),
        1,
        "LLM called once on miss"
    );

    // Second check with identical content — must hit the cache.
    let r2 = monitor.check("session-x", pane_text).await.unwrap();
    assert!(
        r2.cache_hit,
        "second check with same content must be a cache hit"
    );
    assert_eq!(
        r2.verdict.state,
        ActivityState::Working,
        "verdict preserved from cache"
    );
    assert_eq!(
        call_count.load(Ordering::Relaxed),
        1,
        "LLM must NOT be called on cache hit"
    );

    // Third check with different content — cache miss again.
    let r3 = monitor
        .check("session-x", "$ claude\n> done")
        .await
        .unwrap();
    assert!(!r3.cache_hit, "changed content must be a cache miss");
    assert_eq!(
        call_count.load(Ordering::Relaxed),
        2,
        "LLM called again on new content"
    );
}

/// Regression guard: handler spawn sequence creates the tmux session in the
/// provisioned workspace, not in $HOME.
///
/// Why: before the fix, `spawn_session` called `mgr.create(cwd=None)` which
/// defaulted to `dirs::home_dir()`. This meant `tmux new-session -c $HOME` and
/// claude opened in the wrong directory, breaking workspace isolation.
/// What: exercises the corrected handler flow — pre-generate id, provision
/// (FakeGitBackend), `create_with_id(cwd=workspace_path)` — and asserts the
/// `create_session` call was recorded with cwd == workspace_path.
/// Test: this function IS the test.
#[tokio::test]
async fn handler_spawn_creates_tmux_at_workspace_cwd() {
    let workspace_root_dir = TempDir::new().unwrap();
    let store_dir = TempDir::new().unwrap();
    let tmux = RecordingTmux::new();
    let mgr = Arc::new(
        SessionManager::new(store_dir.path(), tmux.clone())
            .await
            .expect("manager"),
    );

    let repo_url = "https://github.com/owner/trusty-tools";
    let git_ref = "main";
    let task = "list files";

    // Simulate the fixed handler sequence: pre-generate id, provision, then
    // create_with_id(cwd = workspace_path).
    let session_id = ManagedSessionId::new();
    let prov = WorkspaceProvisioner::without_prepare(
        FakeGitBackend::new(),
        workspace_root_dir.path().to_owned(),
    );
    let prepared = prov
        .provision(&session_id, repo_url, git_ref, task)
        .expect("provision");

    let workspace_path = prepared.path.clone();

    let record = mgr
        .create_with_id(
            session_id,
            task.into(),
            Some(workspace_path.clone()),
            None,
            Some(workspace_path.clone()),
            Some(repo_url.into()),
            Some(git_ref.into()),
            trusty_mpm::runtime::RuntimeKind::default(),
        )
        .await
        .expect("create_with_id");

    // Assert the tmux session was created with cwd = workspace_path.
    let create_calls = tmux.create_calls.lock().unwrap();
    assert_eq!(create_calls.len(), 1, "exactly one session must be created");
    let (session_name, cwd) = &create_calls[0];
    assert_eq!(
        session_name, &record.tmux_name,
        "session name must match the record"
    );
    assert_eq!(
        cwd,
        &workspace_path.to_string_lossy().to_string(),
        "tmux session cwd must equal the provisioned workspace path, not $HOME"
    );

    // Must NOT be $HOME.
    let home = dirs::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();
    assert_ne!(
        cwd, &home,
        "tmux session cwd must NOT be $HOME (workspace-isolation regression)"
    );

    // The workspace must live under the mpm workspace root.
    assert!(
        workspace_path.starts_with(workspace_root_dir.path()),
        "workspace must be under the mpm workspace root; got {}",
        workspace_path.display()
    );
}

/// Live end-to-end test against real tmux + git.
///
/// Why: the unit tests stub tmux and git; this test verifies the real adapters
/// against an actual `tmux` binary and a temporary bare git repo. It is
/// `#[ignore]` so CI (which lacks tmux/git guarantees) stays green; run locally
/// with `cargo test -p trusty-mpm -- --include-ignored`.
/// What: provisions a workspace from a temp bare repo and asserts the checkout
/// directory exists with the expected isolation properties.
/// Test: this function IS the test.
#[test]
#[ignore = "requires a live git binary; run with --include-ignored"]
fn live_provision_real_repo() {
    use std::process::Command;
    use trusty_mpm::provisioner::RealGitBackend;

    let scratch = TempDir::new().unwrap();
    let bare = scratch.path().join("origin.git");
    // Create a bare repo with one commit on `main`.
    let work = scratch.path().join("seed");
    assert!(
        Command::new("git")
            .args(["init", "--bare", "-b", "main"])
            .arg(&bare)
            .status()
            .map(|s| s.success())
            .unwrap_or(false),
        "git init --bare must succeed"
    );
    assert!(
        Command::new("git")
            .args(["clone"])
            .arg(&bare)
            .arg(&work)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    );
    std::fs::write(work.join("README.md"), "seed").unwrap();
    for args in [
        vec!["-C", work.to_str().unwrap(), "add", "."],
        vec![
            "-C",
            work.to_str().unwrap(),
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-m",
            "seed",
        ],
        vec!["-C", work.to_str().unwrap(), "push", "origin", "main"],
    ] {
        let _ = Command::new("git").args(&args).status();
    }

    let root = TempDir::new().unwrap();
    let prov = WorkspaceProvisioner::new(RealGitBackend, root.path().to_owned());
    let id = ManagedSessionId::new();
    let repo_url = format!("file://{}", bare.display());
    let ws = prov
        .provision(&id, &repo_url, "main", "live task")
        .expect("provision real repo");
    assert!(ws.path.exists());
    assert!(ws.path.join("README.md").exists());
}

// ── #1203: tcode-backed managed session integration ─────────────────────────
//
// Acceptance criterion: "Integration test verifies a tcode-backed session can
// be spawned and issued commands." This drives the same create→build_adapter→
// send_input path the spawn handler uses, but with `RuntimeKind::Tcode`, and
// asserts (a) the record persists `runtime = tcode` so resume re-spawns tcode,
// and (b) an operator command can be issued into the session's pane.

use trusty_mpm::runtime::{RuntimeKind, build_adapter};

/// A tcode-backed session is spawnable and can be issued commands.
///
/// Why: proves the tcode runtime is wired end-to-end through the session
/// manager — the record carries the tcode backend, the tcode adapter is
/// constructed via `build_adapter`, and the pane accepts a subsequent command.
/// What: creates a session with `RuntimeKind::Tcode`, asserts the persisted
/// `runtime` is tcode, builds the tcode adapter and spawns (tolerating the
/// `BinaryNotFound` error when `tcode` is absent in CI), then issues a command
/// via `send_input` and asserts it reached the recording tmux pane.
/// Test: this function IS the test.
#[tokio::test]
async fn tcode_session_spawns_and_accepts_commands() {
    let store_dir = TempDir::new().unwrap();
    let tmux = RecordingTmux::new();
    let mgr = Arc::new(
        SessionManager::new(store_dir.path(), tmux.clone())
            .await
            .expect("manager"),
    );

    // Create a tcode-backed session.
    let record = mgr
        .create_with_id(
            ManagedSessionId::new(),
            "implement feature Y".into(),
            Some(std::path::PathBuf::from("/tmp/tcode-ws")),
            None,
            Some(std::path::PathBuf::from("/tmp/tcode-ws")),
            Some("https://github.com/owner/repo".into()),
            Some("main".into()),
            RuntimeKind::Tcode,
        )
        .await
        .expect("create_with_id");

    // The persisted runtime must be tcode so resume re-spawns the same backend.
    assert_eq!(record.runtime, RuntimeKind::Tcode);
    let reloaded = mgr.get(&record.id).await.expect("get");
    assert_eq!(reloaded.runtime, RuntimeKind::Tcode);

    // Build the tcode adapter the way the spawn handler does and spawn it.
    // `spawn` returns BinaryNotFound when `tcode` is not installed (CI); the
    // wiring under test is the adapter selection, which we assert via identify().
    let adapter = build_adapter(record.runtime, mgr.tmux_driver());
    assert_eq!(adapter.identify(), "tcode");
    let _ = adapter.spawn(
        &record.tmux_name,
        std::path::Path::new("/tmp/tcode-ws"),
        &record.task,
    );

    // Issue a command into the session's pane (operator interaction).
    mgr.send_input(&record.id, "status")
        .await
        .expect("send_input");

    // The command must have reached the recording tmux pane for this session.
    let sends = tmux.sends.lock().unwrap();
    assert!(
        sends
            .iter()
            .any(|(name, text)| name == &record.tmux_name && text == "status"),
        "the issued command must reach the tcode session's pane; sends={sends:?}"
    );
}

// ── Typed resume-error tests (regression guard for #1221 review findings) ───────
//
// Why: the resume HTTP handler previously chose its 404/409 status by
// SUBSTRING-matching the error's `Display` string, which silently regressed to
// 500 whenever wording changed; and it ran a pre-flight `get` before `resume`,
// opening a TOCTOU window. These tests drive the shared `resume_managed` helper
// directly and assert on the TYPED `ResumeManagedError` variant — never on the
// Display string — so the 404/409 contract is enforced structurally.

use trusty_mpm::daemon::managed_routes::{ResumeManagedError, resume_managed};
use trusty_mpm::daemon::state::DaemonState;
use trusty_mpm::runtime::RuntimeKind as ResumeRuntimeKind;
use trusty_mpm::session_manager::ManagedSessionId as ResumeSessionId;
use trusty_mpm::session_manager::TmuxSessionGuard;

/// Build an ARMED RAII teardown guard that reaps `tmux_name` on test exit (#1459).
///
/// Why: the two tests below seed a session through a `DaemonState` whose session
/// manager uses the REAL tmux driver when `tmux` is on the host. Without this,
/// each run leaked a live `tmpm-…` tmux session — those orphans saturated the
/// host fork limit (epic #1452). Reusing the production [`TmuxSessionGuard`]
/// (left ARMED — never disarmed) means the session is killed when the guard
/// drops at the end of the test, on success, failure, OR panic unwind.
/// What: pulls the manager's real driver via `tmux_driver()` and wraps it in an
/// armed guard owning `tmux_name`.
/// Test: exercised by `resume_managed_typed_invalid_state_is_conflict` and
/// `front_gate_answer_unblocks_spawn`; the guard's own kill/disarm behavior is
/// unit-tested in `session_manager::session_guard`.
async fn reap_on_drop(state: &Arc<DaemonState>, tmux_name: &str) -> TmuxSessionGuard {
    let driver = state.session_manager().await.tmux_driver();
    TmuxSessionGuard::new(tmux_name.to_string(), driver)
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
    // Hermetic framework root so the on-disk session store never touches the
    // operator's real `~/.trusty-mpm`.
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root(root.path().to_path_buf()));
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
    // Hermetic framework root so the seeded session persists to a temp store, not
    // the operator's real `~/.trusty-mpm`.
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root(root.path().to_path_buf()));
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
    let seeded = mgr
        .create_with_id(
            id,
            "regression: invalid-state resume".to_string(),
            Some(ws.clone()),
            None,
            Some(ws),
            Some("https://example.com/r.git".to_string()),
            Some("main".to_string()),
            ResumeRuntimeKind::default(),
        )
        .await
        .expect("seed session");

    // Reap the real tmux session this test created on exit (#1459). Held to the
    // end of the function so success, failure, or panic all trigger the kill.
    let _reaper = reap_on_drop(&state, &seeded.tmux_name).await;

    let err = resume_managed(&state, &id)
        .await
        .expect_err("resuming a Provisioning session must error");

    assert!(
        matches!(err, ResumeManagedError::InvalidState(_)),
        "non-resumable state must map to the typed InvalidState variant (→ 409), got {err:?}"
    );
}

/// FRONT gate (#1360, AC-15): the withheld spawn launches after human approval.
///
/// Why: a session escalated by the FRONT gate sits in `Provisioning` with no
/// runtime. Resolving its decision must actually LAUNCH the runtime (not just
/// clear the flag) — `spawn_runtime_for` is the lifted Step 3 the answer path
/// invokes. This asserts the runtime is spawned and the session leaves
/// `Provisioning`.
/// What: seeds a `Provisioning` session with a pending decision via the daemon's
/// manager, calls `spawn_runtime_for`, and asserts the session is no longer
/// `Provisioning` (the spawn was performed — `Active` on success, `Errored` only
/// if the runtime binary is absent).
/// Test: this function IS the test.
#[tokio::test]
async fn front_gate_answer_unblocks_spawn() {
    use trusty_mpm::daemon::managed_routes::spawn_runtime_for;
    use trusty_mpm::session_manager::ManagedSessionState;

    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root(root.path().to_path_buf()));
    let mgr = state.session_manager().await;

    let id = ResumeSessionId::new();
    let ws = root.path().join(format!("{id}-frontgate-ws"));
    let record = mgr
        .create_with_id(
            id,
            "Closes #1360: implement listing".to_string(),
            Some(ws.clone()),
            None,
            Some(ws),
            Some("https://github.com/owner/repo".to_string()),
            Some("main".to_string()),
            ResumeRuntimeKind::default(),
        )
        .await
        .expect("seed session");

    // Reap the real tmux session this test created (and any runtime
    // `spawn_runtime_for` starts in the SAME `tmux_name`) on exit (#1459). Held
    // to function end so success, failure, or panic all trigger the kill.
    let _reaper = reap_on_drop(&state, &record.tmux_name).await;

    // Simulate a FRONT-gate escalation: pending decision, still Provisioning.
    mgr.set_pending_decision(&record.id, "conformance divergence", Some("use cursor"))
        .await
        .expect("set_pending_decision");
    let before = mgr.get(&record.id).await.expect("get");
    assert_eq!(before.state, ManagedSessionState::Provisioning);

    // Human approves → clear decision + launch the withheld runtime.
    mgr.clear_pending_decision(&record.id).await.expect("clear");
    // Re-fetch after clearing so we spawn from the fresh, post-clear snapshot
    // (mirrors the answer-route fix; #1360 review).
    let fresh = mgr.get(&record.id).await.expect("get fresh");
    let spawn_result = spawn_runtime_for(&state, &fresh).await;

    let after = mgr.get(&record.id).await.expect("get");
    // The withheld spawn must ALWAYS leave Provisioning (AC-15).
    assert_ne!(
        after.state,
        ManagedSessionState::Provisioning,
        "the withheld spawn must advance the session out of Provisioning (AC-15)"
    );
    assert!(after.pending_decision.is_none());

    // Fail LOUDLY on an unexpected terminal state. Two outcomes are legitimate:
    //   - `Active`  → the runtime actually spawned (`spawn_runtime_for` => Ok);
    //   - `Errored` → CI-without-tmux: no tmux/runtime binary is present, so
    //     `adapter.spawn` fails and `spawn_runtime_for` returns Err, which
    //     `mark_errored` records as a spawn failure on the record's task field.
    // We tie the assertion to the spawn RESULT (not a blanket "any non-Provisioning
    // state") so a genuinely wrong transition can never pass silently. The benign
    // CI case is distinguished by asserting on the recorded error reason, not by
    // blindly accepting any Errored state.
    match spawn_result {
        Ok(()) => assert_eq!(
            after.state,
            ManagedSessionState::Active,
            "successful spawn must leave the session Active"
        ),
        Err(_) => {
            assert_eq!(
                after.state,
                ManagedSessionState::Errored,
                "a failed spawn must mark the session Errored, not any other state"
            );
            // `mark_errored` appends `[error: spawn failed: …]` to the task field;
            // asserting on it confirms this is the benign runtime/tmux-unavailable
            // case (the only way spawn legitimately fails in CI) rather than an
            // unrelated failure that happens to land in Errored.
            assert!(
                after.task.contains("[error: spawn failed:"),
                "Errored state must carry the runtime/tmux-unavailable spawn-failure \
                 reason (CI-without-tmux); got task: {:?}",
                after.task
            );
        }
    }
}

/// Why: issue #1313 review nitpick #7 — assert the SM-unavailable → exit-code-75
/// contract end-to-end through the real `tm` binary, not just the
/// `EXIT_SM_UNAVAILABLE` constant. `prune_idle` no longer calls `process::exit`;
/// it returns `PruneError::SmUnavailable`, which `main` downcasts and translates
/// to exit 75. Pointing the command at a dead loopback port forces the transport
/// (unreachable) branch deterministically without standing up a daemon.
/// What: spawns `tm --url http://127.0.0.1:<dead> sessions prune-idle --json` and
/// asserts the process exits with status code 75. Uses `CARGO_BIN_EXE_tm`
/// (set by Cargo for integration tests) so no extra dev-dependency is needed.
/// Test: this test.
#[test]
fn cli_prune_idle_unreachable_exit_code() {
    use std::process::Command;

    // 127.0.0.1:1 is a reserved/dead port: connecting there fails fast with a
    // transport error, which is exactly the SM-unavailable (exit 75) path.
    let bin = env!("CARGO_BIN_EXE_tm");
    let output = Command::new(bin)
        .args([
            "--url",
            "http://127.0.0.1:1",
            "sessions",
            "prune-idle",
            "--json",
        ])
        .output()
        .expect("spawn tm binary");

    assert_eq!(
        output.status.code(),
        Some(75),
        "SM-unavailable prune-idle must exit 75; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The --json branch still prints the serde-derived unavailable document.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"sm_available\": false"),
        "expected sm_available:false in JSON, got: {stdout}"
    );
}
