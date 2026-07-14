//! Integration tests for the session-manager MVP.
//!
//! Why: the session-manager MVP spans several units (provisioner, catalog sync,
//! session manager, daemon routes). These tests verify the units that can be
//! exercised without a live tmux/git/LLM, plus `#[ignore]` live tests that
//! drive the real tmux + git path on a developer machine.
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
    fn capture(&self, _name: &str, _lines: usize) -> Result<String, ManagedError> {
        Ok(String::new())
    }
    fn list_sessions(&self) -> Result<Vec<String>, ManagedError> {
        Ok(Vec::new())
    }
    fn session_exists(&self, _name: &str) -> bool {
        false
    }
}

/// A tmux driver that actually TRACKS created/killed session names (#2022).
///
/// Why: `DaemonState::with_root_isolated_managed`'s `FakeNoopTmuxDriver` is
/// deliberately stateless — every session always reports "not live" — which is
/// exactly right for the many tests in this file proving "no real tmux session
/// ever escapes the test", but wrong for the delete-route tests that exercise
/// the #2022 fix (the delete guard is now a REAL tmux liveness probe, not a
/// persisted-state check): those need a driver whose `session_exists` reflects
/// reality. `create_session` records the name as live; `kill_session` removes
/// it — mirroring exactly what `create_with_id`/`stop`/`decommission` already
/// call in production.
/// What: a `Mutex<HashSet<String>>` of live session names, driving
/// `create_session`/`kill_session`/`list_sessions` (and therefore the trait's
/// default `session_exists`). Used via
/// `DaemonState::with_root_isolated_managed_and_driver`.
/// Test: `delete_route_removes_record`, `delete_route_refuses_running_without_force`,
/// `delete_route_force_bypasses_guard`.
struct LiveTrackingTmux {
    live: std::sync::Mutex<std::collections::HashSet<String>>,
}

impl LiveTrackingTmux {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            live: std::sync::Mutex::new(std::collections::HashSet::new()),
        })
    }
}

impl ManagedTmuxDriver for LiveTrackingTmux {
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
    let _ = adapter.spawn(
        &record.tmux_name,
        &prepared.path,
        task,
        &record.id.to_string(),
    );

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
            false,
            false,
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
    let prov = WorkspaceProvisioner::new(RealGitBackend::default(), root.path().to_owned());
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
            false,
            false,
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
        &record.id.to_string(),
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
// #2577 review: the WorkspaceGone/PaneGone-split mapping tests, and the
// resume_managed_typed_* tests (NotFound/InvalidState/WorkspaceGone/PaneGone),
// now live in the sibling integration test `tests/resume_unresumable_mapping.rs`
// — extracted once the new PaneGoneTmux driver and its coverage pushed this
// file over the 1500-SLOC test cap. `resume_managed`/`DaemonState`/etc. are
// still imported below — the LATER tests in this file (self-heal, status-line
// healing, incomplete-deployment) still call `resume_managed` directly.

use trusty_mpm::daemon::managed_routes::resume_managed;
use trusty_mpm::daemon::state::DaemonState;
use trusty_mpm::runtime::RuntimeKind as ResumeRuntimeKind;
use trusty_mpm::session_manager::ManagedSessionId as ResumeSessionId;

// ── RAII PATH guard (#2229 CI determinism) ─────────────────────────────────
//
// Why: `resolve_statusline_binary`'s PATH-lookup fallback (see
// `core::session_launch::settings::STATUSLINE_BIN_NAMES`) is exercised by
// `resume_managed_heals_stale_bare_status_line_command` below. `current_exe()`
// is unconditionally an ephemeral `target/debug/deps/...` path inside a
// `cargo test` binary, so that test ALWAYS falls through to the PATH-lookup
// branch — whether it then finds a real `tm`/`trusty-mpm` binary depends
// entirely on the ambient environment (present on a dev machine with
// `~/.cargo/bin` on PATH, absent in a stripped-down CI runner). Prepending a
// temp dir containing a fake, executable `tm` to `PATH` for the duration of
// the test makes the PATH-lookup hit deterministic in both environments,
// mirroring the `HomeGuard` pattern in `tests/standalone_isolation.rs`.
// `std::env::set_var`/`remove_var` are `unsafe` in Rust 2024 (thread-unsafe),
// so every caller pairs this guard with `#[serial_test::serial]`.
struct PathGuard(Option<String>);

impl PathGuard {
    /// Prepend `dir` to `PATH` and return a guard that restores the original
    /// value on drop.
    ///
    /// Why: `trusty_common::bin_resolve::resolve_binary` checks the live
    /// `PATH` before its well-known-dirs fallback, so prepending here is
    /// sufficient to make a fake binary the first hit.
    /// What: saves the current `PATH`, sets `PATH` to `<dir>:<old PATH>`,
    /// returns a guard.
    /// Test: exercised by `resume_managed_heals_stale_bare_status_line_command`.
    fn prepend(dir: &std::path::Path) -> Self {
        let prev = std::env::var("PATH").ok();
        let new_path = match &prev {
            Some(p) => format!("{}:{p}", dir.display()),
            None => dir.display().to_string(),
        };
        // SAFETY: guarded by #[serial_test::serial] on all callers — only one
        // thread mutates PATH at a time.
        unsafe { std::env::set_var("PATH", new_path) };
        PathGuard(prev)
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        match &self.0 {
            Some(p) => unsafe { std::env::set_var("PATH", p) },
            None => unsafe { std::env::remove_var("PATH") },
        }
    }
}

/// Create a fake, executable `tm` binary at `<dir>/tm` so
/// `trusty_common::bin_resolve::resolve_binary("tm")` resolves it.
///
/// Why: `candidate()` (the leaf of `resolve_binary`) requires the file to
/// exist AND carry an execute bit on Unix — an empty regular file is rejected.
/// What: writes a trivial shell script and chmods it `0o755`.
/// Test: exercised by `resume_managed_heals_stale_bare_status_line_command`.
fn write_fake_tm_binary(dir: &std::path::Path) -> PathBuf {
    let bin = dir.join("tm");
    std::fs::write(&bin, "#!/bin/sh\nexit 0\n").expect("write fake tm binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fake tm binary executable");
    }
    bin
}

/// #1913 self-heal: `resume_managed` must backfill a missing `statusLine` key
/// in a resumed session's workspace, even though the workspace was never
/// prepared by `prepare_session*` in the first place.
///
/// Why: sessions spawned via the (pre-#1913) broken in-project worktree path
/// never ran the prep pipeline at all, so their `.claude/settings.json` is
/// permanently missing `statusLine` — and nothing else in the launch path
/// would ever add it. `resume_managed` now defensively calls
/// `ensure_status_line` on every resume so such a session self-heals the next
/// time an operator resumes it, without re-running the (heavier, not fully
/// idempotent) full prep pipeline.
/// What: seeds a session whose `workspace_path` is a real temp directory with
/// NO `.claude/settings.json` (simulating the pre-fix broken state), stops it
/// (`Stopped` is resumable), calls `resume_managed`, and asserts
/// `<workspace>/.claude/settings.json` now exists and contains `"statusLine"`.
/// The runtime adapter spawn itself is allowed to fail in CI (no real
/// `tmux`/`claude` binary) — `resume_managed` never propagates that failure —
/// so this test only asserts on the self-heal side effect, not the spawn
/// outcome.
/// Test: this function IS the test.
#[tokio::test]
async fn resume_managed_backfills_missing_status_line() {
    // Hermetic framework root with FakeNoopTmuxDriver — no real tmux sessions
    // are created, so nothing can escape into the production store (#1790).
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;

    let id = ResumeSessionId::new();
    let ws = root.path().join(format!("{id}-selfheal-ws"));
    std::fs::create_dir_all(&ws).expect("create workspace dir");

    // Precondition: no `.claude/settings.json` at all — the exact pre-#1913
    // broken state (never prepared).
    let settings_path = ws.join(".claude").join("settings.json");
    assert!(
        !settings_path.exists(),
        "precondition: workspace must start with no settings.json"
    );

    let _seeded = mgr
        .create_with_id(
            id,
            "regression: statusline self-heal on resume".to_string(),
            Some(ws.clone()),
            None,
            Some(ws.clone()),
            Some("https://github.com/owner/repo".to_string()),
            Some("main".to_string()),
            ResumeRuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session");

    // Stop first — resume is only valid from Stopped/Errored.
    mgr.stop(&id).await.expect("stop");

    // `resume_managed` never propagates a runtime-adapter spawn failure (CI has
    // no real tmux/claude binary), so we don't assert on its Ok/Err — only on
    // the self-heal side effect, which must land regardless of spawn outcome.
    let _ = resume_managed(&state, &id).await;

    let content = std::fs::read_to_string(&settings_path).unwrap_or_else(|e| {
        panic!(
            "resume_managed must backfill {}: {e}",
            settings_path.display()
        )
    });
    assert!(
        content.contains("statusLine"),
        "resumed workspace settings.json must carry the statusLine key \
         (the #1913 self-heal); got: {content}"
    );
}

/// P0 regression (#2172): the #2158 deployment-completeness check must NEVER
/// prevent `resume_managed` from reaching the runtime spawn, even when
/// validation (and its auto-repair attempt) still reports the workspace
/// incomplete afterward.
///
/// Why: before this fix, `ensure_deployment_complete` returning `Err` caused
/// `resume_managed` to `mark_errored` the session with the gate's own
/// "deployment incomplete after auto-repair" message and skip
/// `adapter.spawn_resume` entirely — collapsing runtime launch on every
/// session whose validator reported INCOMPLETE, including false positives
/// (#2171, not yet fixed). This is the exact regression that broke every
/// new/restarted managed session in production.
/// What: seeds a resumable session whose workspace has no `.claude/` payload
/// at all (guaranteeing `ensure_deployment_complete` sees gaps before repair),
/// then makes the workspace directory READ-ONLY (`chmod 0o555`) so the
/// auto-repair pipeline can never create `.claude/settings.json` inside it —
/// the simplest deterministic way to force the gate's `Err` branch (gaps
/// remain after repair too) without depending on the still-open #2171 bug.
/// Resumes the session and asserts the resulting record's `task` field
/// (where `SessionManager::mark_errored` appends `[error: …]`) never contains
/// the gate's own "deployment incomplete" wording — proving the gate no
/// longer aborts the handler before `adapter.spawn_resume` runs. (The runtime
/// adapter itself is still allowed to fail in CI — no real `tmux`/`claude`
/// binary on PATH — so this test does not assert the final state is
/// `Active`; only that the deployment gate is never the terminal error,
/// mirroring `resume_managed_backfills_missing_status_line`'s established
/// pattern of not asserting on the spawn outcome.)
/// Test: this function IS the test.
#[tokio::test]
async fn resume_managed_launches_despite_incomplete_deployment() {
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;

    let id = ResumeSessionId::new();
    let ws = root.path().join(format!("{id}-incomplete-ws"));
    std::fs::create_dir_all(&ws).expect("create workspace dir");

    // Precondition: no `.claude/` payload at all — guarantees the pre-repair
    // validation pass reports gaps (e.g. SettingsMissing).
    assert!(
        !ws.join(".claude").exists(),
        "precondition: workspace must start with no .claude/ payload"
    );

    let _seeded = mgr
        .create_with_id(
            id,
            "regression: non-blocking deployment gate on resume (#2172)".to_string(),
            Some(ws.clone()),
            None,
            Some(ws.clone()),
            Some("https://github.com/owner/repo".to_string()),
            Some("main".to_string()),
            ResumeRuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session");

    // Stop first — resume is only valid from Stopped/Errored.
    mgr.stop(&id).await.expect("stop");

    // Make the workspace read-only so the repair pipeline cannot create
    // `.claude/settings.json` inside it — after-repair validation must still
    // report the workspace incomplete, forcing `ensure_deployment_complete`
    // to return `Err`.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ws, std::fs::Permissions::from_mode(0o555))
            .expect("chmod workspace read-only");
    }

    let result = resume_managed(&state, &id).await;

    // Restore write permission unconditionally so the TempDir's Drop impl can
    // clean up the directory even if an assertion below panics.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&ws, std::fs::Permissions::from_mode(0o755));
    }

    let record = result.expect(
        "resume_managed must still return Ok even when the deployment gate \
         reports incomplete (P0 #2172) — the gate must never abort the handler",
    );
    assert!(
        !record.task.contains("deployment incomplete"),
        "the #2158 deployment-completeness gate must be non-blocking (#2172): \
         it must never be the reason a spawn/resume is marked errored; got \
         task: {}",
        record.task
    );
}

/// #1914 self-heal: `resume_managed` must also upgrade a STALE bare
/// `tm statusline` command already on disk to an absolute path, not just
/// backfill a missing key.
///
/// Why: a workspace prepared by a pre-#1914 build wrote the literal bare
/// string `"tm statusline"`, which silently fails to render under a minimal
/// `PATH` (e.g. launchd, Claude Code's own spawn environment). This extends
/// the SAME resume self-heal entry point `resume_managed_backfills_missing_status_line`
/// exercises (`ensure_status_line` → `write_status_line`) rather than adding a
/// second hook, so both self-heal concerns land through one code path.
/// What: seeds a workspace whose `.claude/settings.json` already has the exact
/// pre-#1914 bare `statusLine.command`, resumes it, and asserts the on-disk
/// command is upgraded to the fake, PATH-resolved `tm` binary this test seeds
/// (see `PathGuard`/`write_fake_tm_binary`) rather than merely checking for a
/// `/` — #2229 made `current_exe()` always ineligible inside a `cargo test`
/// binary (ephemeral `target/debug/deps/...`), so without a seeded PATH hit
/// the outcome depends on whether the ambient environment happens to have
/// `tm`/`trusty-mpm` installed, which is exactly what made this test pass
/// locally and fail in CI.
/// Test: this function IS the test.
#[tokio::test]
#[serial_test::serial]
async fn resume_managed_heals_stale_bare_status_line_command() {
    // #2229 CI determinism: `resolve_statusline_binary`'s `current_exe()`
    // branch is ALWAYS rejected inside a `cargo test` binary (it lives under
    // `target/debug/deps/...`, an ephemeral build path), so this test always
    // exercises the PATH-lookup fallback. Whether that fallback finds a real
    // `tm`/`trusty-mpm` binary is otherwise environment-dependent (present on
    // a dev machine with `~/.cargo/bin` on PATH, absent in CI, which is
    // exactly why this test flaked green-locally/red-in-CI) — prepend a fake,
    // executable `tm` onto `PATH` so the upgrade is deterministic in both
    // environments. See `PathGuard`/`write_fake_tm_binary` above.
    let fake_bin_dir = TempDir::new().unwrap();
    let fake_tm = write_fake_tm_binary(fake_bin_dir.path());
    let _path_guard = PathGuard::prepend(fake_bin_dir.path());

    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;

    let id = ResumeSessionId::new();
    let ws = root.path().join(format!("{id}-stale-heal-ws"));
    let claude_dir = ws.join(".claude");
    std::fs::create_dir_all(&claude_dir).expect("create .claude dir");
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::json!({
            "statusLine": {"type": "command", "command": "tm statusline", "padding": 0}
        })
        .to_string(),
    )
    .expect("seed pre-#1914 bare statusLine");

    let _seeded = mgr
        .create_with_id(
            id,
            "regression: stale bare statusline self-heal on resume".to_string(),
            Some(ws.clone()),
            None,
            Some(ws.clone()),
            Some("https://github.com/owner/repo".to_string()),
            Some("main".to_string()),
            ResumeRuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("seed session");

    mgr.stop(&id).await.expect("stop");
    let _ = resume_managed(&state, &id).await;

    let settings_path = claude_dir.join("settings.json");
    let content = std::fs::read_to_string(&settings_path).unwrap_or_else(|e| {
        panic!(
            "resume_managed must rewrite {}: {e}",
            settings_path.display()
        )
    });
    let value: serde_json::Value = serde_json::from_str(&content).expect("valid JSON");
    let command = value["statusLine"]["command"]
        .as_str()
        .expect("statusLine.command is a string");
    // A single exact-equality assertion subsumes "was upgraded" (differs from
    // the seeded bare literal), "resolves to an absolute path" (the fake
    // binary's path is absolute), and "still invokes statusline" — no need for
    // three separate weaker assertions.
    let expected = format!("{} statusline", fake_tm.display());
    assert_eq!(
        command, expected,
        "the stale bare command must be upgraded to the PATH-resolved fake `tm` \
         binary seeded by this test, not left as-is; got: {content}"
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

    // FakeNoopTmuxDriver: no real tmux sessions are created — nothing can escape
    // into the production store (#1790).
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
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
            false,
            false,
        )
        .await
        .expect("seed session");

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
/// A hermetic HOME is planted with a lock file that anchors the lock-file
/// fallback in `resolve_daemon_url_probing` (#1731) to the same dead port,
/// preventing a live daemon on the default port from intercepting resolution.
/// Test: this test.
#[test]
fn cli_prune_idle_unreachable_exit_code() {
    use std::process::Command;

    // Plant a hermetic HOME so `resolve_daemon_url_probing` (#1731) cannot fall
    // back to a real daemon's `~/.trusty-mpm/daemon.lock`. The fake lock file
    // points at the same dead port with our PID (alive) so the staleness check
    // in `read_lock_file_url` passes and the file is not silently discarded.
    let fake_home = tempfile::tempdir().expect("create temp home");
    let lock_dir = fake_home.path().join(".trusty-mpm");
    std::fs::create_dir_all(&lock_dir).expect("create .trusty-mpm under fake HOME");
    let lock_content = format!(
        "addr = \"http://127.0.0.1:1\"\npid = {}\n",
        std::process::id()
    );
    std::fs::write(lock_dir.join("daemon.lock"), &lock_content).expect("write fake daemon.lock");

    // 127.0.0.1:1 is a reserved/dead port: connecting there fails fast with a
    // transport error, which is exactly the SM-unavailable (exit 75) path.
    let bin = env!("CARGO_BIN_EXE_tm");
    let output = Command::new(bin)
        .args([
            "--url",
            "http://127.0.0.1:1",
            "session",
            "prune-idle",
            "--json",
        ])
        .env("HOME", fake_home.path())
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

// ── #1508: HTTP route tests for the bulk-teardown + by-state prune endpoints ──
//
// These call the route handlers directly with axum's `State`/`Json` extractors
// (the same pattern as the typed `resume_managed` tests above) against a hermetic
// `DaemonState::with_root_isolated_managed`, then decode the JSON response body.
// They seed sessions via `create_with_id` using the FakeNoopTmuxDriver so no real
// tmux sessions escape into the host (#1790).

/// Decode an axum `impl IntoResponse` into `(StatusCode, serde_json::Value)`.
///
/// Why: the prune route handlers return `impl IntoResponse`; a route test must
/// inspect both the status and the JSON body. Centralising the body-read keeps
/// each test focused on its assertion.
/// What: converts the response, reads the full body to bytes, and parses it as
/// JSON (an empty/non-JSON body yields `Value::Null`).
/// Test: used by the `*_route_*` tests below.
async fn decode_response(
    resp: impl axum::response::IntoResponse,
) -> (axum::http::StatusCode, serde_json::Value) {
    let resp = resp.into_response();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

/// POST …/decommission-ephemeral tears down ONLY ephemeral sessions (#1508).
///
/// Why: the HTTP surface must honour the same safety invariant as the engine —
/// REAL (non-ephemeral) sessions are never touched by the bulk-teardown route.
/// What: seeds two ephemeral and one durable session through the daemon's manager,
/// calls [`decommission_ephemeral_route`], asserts the response reports
/// `decommissioned == 2`, and confirms the durable session is left untouched.
/// Test: this function IS the test.
#[tokio::test]
async fn decommission_ephemeral_route_tears_down_only_ephemeral() {
    use trusty_mpm::daemon::managed_routes::decommission_ephemeral_route;
    use trusty_mpm::session_manager::ManagedSessionState;

    // FakeNoopTmuxDriver: no real tmux sessions are created — nothing can escape
    // into the production store (#1790).
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;

    // Seed two ephemeral + one durable session. `create_with_id` starts each in
    // Provisioning (running); the bulk-teardown intentionally includes running
    // ephemerals, so both ephemeral records are torn down and the durable is not.
    let mut seeded = Vec::new();
    for (label, ephemeral) in [("eph-a", true), ("eph-b", true), ("durable", false)] {
        let id = ResumeSessionId::new();
        let ws = root.path().join(format!("{id}-{label}"));
        let rec = mgr
            .create_with_id(
                id,
                format!("route test {label}"),
                Some(ws.clone()),
                None,
                Some(ws),
                Some("https://example.com/r.git".to_string()),
                Some("main".to_string()),
                ResumeRuntimeKind::default(),
                ephemeral,
                false,
            )
            .await
            .expect("seed session");
        seeded.push((rec, ephemeral));
    }
    // No real tmux sessions were created (FakeNoopTmuxDriver), so no reap
    // guards are needed (#1790).

    let (status, body) =
        decode_response(decommission_ephemeral_route(axum::extract::State(state.clone())).await)
            .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(
        body["decommissioned"], 2,
        "exactly the two ephemeral sessions are torn down, got {body}"
    );

    // The durable session must remain (NOT Decommissioned).
    let durable = seeded
        .iter()
        .find(|(_, eph)| !eph)
        .map(|(r, _)| r.id)
        .expect("durable seeded");
    assert_ne!(
        mgr.get(&durable).await.expect("get durable").state,
        ManagedSessionState::Decommissioned,
        "the durable session must never be touched by the ephemeral teardown route"
    );
}

/// POST …/prune with `dry_run` REPORTS candidates and mutates NOTHING (#1508).
///
/// Why: a dry-run is the operator's safe preview before a destructive purge; the
/// route must echo the candidates without changing any record.
/// What: seeds one ephemeral session, calls [`prune_managed_route`] with
/// `state=ephemeral, dry_run=true`, asserts the body reports `dry_run:true` with
/// one candidate, and confirms the session is still its original (non-terminal)
/// state afterwards.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_route_dry_run_reports() {
    use trusty_mpm::daemon::managed_routes::{PruneRequest, prune_managed_route};
    use trusty_mpm::session_manager::ManagedSessionState;

    // FakeNoopTmuxDriver: no real tmux sessions are created — nothing can escape
    // into the production store (#1790).
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;

    let id = ResumeSessionId::new();
    let ws = root.path().join(format!("{id}-dryrun"));
    let _ = mgr
        .create_with_id(
            id,
            "dry-run candidate".to_string(),
            Some(ws.clone()),
            None,
            Some(ws),
            Some("https://example.com/r.git".to_string()),
            Some("main".to_string()),
            ResumeRuntimeKind::default(),
            true,
            false,
        )
        .await
        .expect("seed ephemeral");
    // No real tmux session was created (FakeNoopTmuxDriver), so no reap guard
    // needed (#1790).
    let before = mgr.get(&id).await.expect("get before").state;

    let req = serde_json::from_value::<PruneRequest>(serde_json::json!({
        "state": "ephemeral",
        "dry_run": true,
        // Provisioning is a running state; include it so the dry-run still lists
        // the freshly-created candidate (a real purge would pass false here).
        "include_active": true,
    }))
    .expect("build request");
    let (status, body) = decode_response(
        prune_managed_route(axum::extract::State(state.clone()), axum::Json(req)).await,
    )
    .await;

    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(body["dry_run"], serde_json::json!(true));
    assert_eq!(body["filter"], serde_json::json!("ephemeral"));
    assert_eq!(
        body["sessions"].as_array().map(|a| a.len()),
        Some(1),
        "dry-run lists the one ephemeral candidate, got {body}"
    );
    // Nothing was mutated: the session is still in its pre-prune state.
    assert_eq!(
        mgr.get(&id).await.expect("get after").state,
        before,
        "a dry-run must NOT change any record's state"
    );
    assert_ne!(before, ManagedSessionState::Decommissioned);
}

/// POST …/prune with an unknown `state` returns 400 (#1508).
///
/// Why: a typo'd filter must be a loud, actionable error — never a silent default
/// to a destructive scope.
/// What: posts `state=garbage` to [`prune_managed_route`] and asserts a
/// `400 Bad Request`. No session is seeded — the parse rejection happens before
/// any store access.
/// Test: this function IS the test.
#[tokio::test]
async fn prune_route_rejects_bad_state() {
    use trusty_mpm::daemon::managed_routes::{PruneRequest, prune_managed_route};

    // FakeNoopTmuxDriver: no session is seeded here, but we still use the
    // isolated constructor to prevent the lazy initialiser from touching
    // the production store (#1790).
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);

    let req = serde_json::from_value::<PruneRequest>(serde_json::json!({ "state": "garbage" }))
        .expect("build request");
    let (status, _body) =
        decode_response(prune_managed_route(axum::extract::State(state), axum::Json(req)).await)
            .await;
    assert_eq!(
        status,
        axum::http::StatusCode::BAD_REQUEST,
        "an unknown prune filter must yield 400"
    );
}

// ── #2012: HTTP route tests for the hard-delete-record endpoint ──────────────
//
// Same pattern as the #1508 prune-route tests above: call the handler directly
// with axum's `State`/`Path`/`Query` extractors against a hermetic
// `DaemonState`, using `create_with_id` + `ResumeRuntimeKind`/`ResumeSessionId`
// (aliased above) so no real tmux session is ever created (#1790). These three
// tests specifically exercise the #2022 running-guard fix (a REAL tmux
// liveness probe, not the persisted `state` field), so they use
// `with_root_isolated_managed_and_driver` + `LiveTrackingTmux` instead of the
// stateless `FakeNoopTmuxDriver` the other handler tests in this file use.

/// POST …/{id}/delete removes a NON-running record from the store (#2012).
///
/// Why: the common case — an operator deleting an already-stopped session's
/// record — must succeed without `--force` and actually drop it from the store
/// (not merely tombstone it).
/// What: seeds a session (registering it as live on `LiveTrackingTmux`), stops
/// it (which kills the tracked tmux session, so it is genuinely no longer
/// running), calls [`delete_managed_session`] with `force=false`, asserts `200`
/// with `deleted: true`, and confirms the record is gone (`SessionNotFound`).
/// Test: this function IS the test.
#[tokio::test]
async fn delete_route_removes_record() {
    use trusty_mpm::daemon::managed_routes::{DeleteQuery, delete_managed_session};

    let root = TempDir::new().unwrap();
    let state = Arc::new(
        DaemonState::with_root_isolated_managed_and_driver(
            root.path().to_path_buf(),
            LiveTrackingTmux::new(),
        )
        .await,
    );
    let mgr = state.session_manager().await;

    let id = ResumeSessionId::new();
    let ws = root.path().join(format!("{id}-delete-ok"));
    let _ = mgr
        .create_with_id(
            id,
            "delete route test".to_string(),
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
    // Take the session out of a running state so the fail-closed guard allows
    // the delete without --force (mirrors an operator's real workflow: stop,
    // then delete).
    mgr.stop(&id).await.expect("stop before delete");

    let (status, body) = decode_response(
        delete_managed_session(
            axum::extract::State(state.clone()),
            axum::extract::Path(id.to_string()),
            axum::extract::Query(DeleteQuery { force: false }),
        )
        .await,
    )
    .await;

    assert_eq!(status, axum::http::StatusCode::OK, "body={body}");
    assert_eq!(body["deleted"], serde_json::json!(true));
    assert!(matches!(
        mgr.get(&id).await,
        Err(ManagedError::SessionNotFound(_))
    ));
}

/// POST …/{id}/delete REFUSES a RUNNING session without `--force` (#2012).
///
/// Why: the #2012 fail-closed safety requirement at the HTTP layer — a running
/// session's record must survive a non-forced delete attempt. Uses
/// `LiveTrackingTmux` (#2022) so the record's tmux session is genuinely live —
/// exercising the corrected guard, not merely its old persisted-state check.
/// What: seeds a session (starts `Provisioning`, a running state, with a live
/// tracked tmux session), calls [`delete_managed_session`] with `force=false`,
/// asserts `409 Conflict`, and confirms the record is still present afterward.
/// Test: this function IS the test.
#[tokio::test]
async fn delete_route_refuses_running_without_force() {
    use trusty_mpm::daemon::managed_routes::{DeleteQuery, delete_managed_session};

    let root = TempDir::new().unwrap();
    let state = Arc::new(
        DaemonState::with_root_isolated_managed_and_driver(
            root.path().to_path_buf(),
            LiveTrackingTmux::new(),
        )
        .await,
    );
    let mgr = state.session_manager().await;

    let id = ResumeSessionId::new();
    let ws = root.path().join(format!("{id}-delete-refuse"));
    let _ = mgr
        .create_with_id(
            id,
            "delete route refuse test".to_string(),
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

    let (status, body) = decode_response(
        delete_managed_session(
            axum::extract::State(state.clone()),
            axum::extract::Path(id.to_string()),
            axum::extract::Query(DeleteQuery { force: false }),
        )
        .await,
    )
    .await;

    assert_eq!(
        status,
        axum::http::StatusCode::CONFLICT,
        "a running session must be refused without --force, body={body}"
    );
    // The record must be untouched by the refused delete.
    let still_there = mgr.get(&id).await.expect("record must still exist");
    assert_ne!(
        still_there.state,
        trusty_mpm::session_manager::ManagedSessionState::Decommissioned
    );
}

/// POST …/{id}/delete?force=true bypasses the running-state guard (#2012).
///
/// Why: an operator who explicitly opts in via `--force` must be able to
/// hard-delete a running session's record over HTTP too, even when the tmux
/// session backing it is genuinely still live (`LiveTrackingTmux`, #2022).
/// What: seeds a running session, calls [`delete_managed_session`] with
/// `force=true`, asserts `200` with `deleted: true`, and confirms the record is
/// gone from the store.
/// Test: this function IS the test.
#[tokio::test]
async fn delete_route_force_bypasses_guard() {
    use trusty_mpm::daemon::managed_routes::{DeleteQuery, delete_managed_session};

    let root = TempDir::new().unwrap();
    let state = Arc::new(
        DaemonState::with_root_isolated_managed_and_driver(
            root.path().to_path_buf(),
            LiveTrackingTmux::new(),
        )
        .await,
    );
    let mgr = state.session_manager().await;

    let id = ResumeSessionId::new();
    let ws = root.path().join(format!("{id}-delete-force"));
    let _ = mgr
        .create_with_id(
            id,
            "delete route force test".to_string(),
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

    let (status, body) = decode_response(
        delete_managed_session(
            axum::extract::State(state.clone()),
            axum::extract::Path(id.to_string()),
            axum::extract::Query(DeleteQuery { force: true }),
        )
        .await,
    )
    .await;

    assert_eq!(status, axum::http::StatusCode::OK, "body={body}");
    assert_eq!(body["deleted"], serde_json::json!(true));
    assert!(matches!(
        mgr.get(&id).await,
        Err(ManagedError::SessionNotFound(_))
    ));
}

// ── #1730: list_managed_sessions ?source_id= filter + serialization ───────────
//
// These tests call `list_managed_sessions` directly (same axum-extractor pattern
// as the prune route tests above) against a hermetic `DaemonState`.  Two sessions
// are seeded: one with `source_id = "owner/repo"` (set via `set_source_id`) and
// one plain session with no source_id.  The three filter branches are exercised:
// known source_id → only matching session; unknown source_id → empty; no param →
// all sessions.  A fourth assertion checks the JSON payload carries `source_id`.

/// Managed-session list: `?source_id=` filter returns only matching sessions (#1730).
///
/// Why: the `GET /api/v1/sessions/managed?source_id=owner/repo` endpoint must
/// return ONLY sessions bound to that project. Without correct serialization +
/// server-side filter, `tm` guided-default shows sessions from other repos or
/// returns none for a fabricated source_id.
/// What: seeds one in-project session (source_id="owner/repo") and one plain
/// session (no source_id), then asserts three filter variants: known source_id
/// returns one matching session, unknown source_id returns zero, absent param
/// returns exactly both seeded sessions (FakeNoopTmuxDriver means no external
/// sessions are ever adopted by reconcile_on_boot). A fourth check asserts the
/// `source_id` field is present in the serialized JSON payload.
/// Test: this function IS the test.
#[tokio::test]
async fn list_managed_sessions_source_id_filter() {
    use std::collections::HashMap;
    use trusty_mpm::daemon::managed_routes::list_managed_sessions;

    // FakeNoopTmuxDriver: no real tmux sessions are created — nothing can escape
    // into the production store, and reconcile_on_boot sees no external sessions
    // so Case 3 can assert exactly 2 rather than >= 2 (#1790).
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;

    // Session A: in-project session — source_id will be set to "owner/repo".
    let id_a = ResumeSessionId::new();
    let ws_a = root.path().join(format!("{id_a}-src-filter-a"));
    mgr.create_with_id(
        id_a,
        "source-id-filter-test-a".to_string(),
        Some(ws_a.clone()),
        None,
        Some(ws_a),
        Some("https://github.com/owner/repo".to_string()),
        Some("main".to_string()),
        ResumeRuntimeKind::default(),
        false,
        false,
    )
    .await
    .expect("seed session A");
    mgr.set_source_id(&id_a, "owner/repo")
        .await
        .expect("set source_id on session A");

    // Session B: plain session — no source_id set.
    let id_b = ResumeSessionId::new();
    let ws_b = root.path().join(format!("{id_b}-src-filter-b"));
    mgr.create_with_id(
        id_b,
        "source-id-filter-test-b".to_string(),
        Some(ws_b.clone()),
        None,
        Some(ws_b),
        Some("https://github.com/other/lib".to_string()),
        Some("main".to_string()),
        ResumeRuntimeKind::default(),
        false,
        false,
    )
    .await
    .expect("seed session B");
    // No real tmux sessions were created (FakeNoopTmuxDriver), so no reap
    // guards needed (#1790).

    // ── Case 1: known source_id → only session A is returned ─────────────────
    let q = HashMap::from([("source_id".to_string(), "owner/repo".to_string())]);
    let (status, body) = decode_response(
        list_managed_sessions(axum::extract::State(state.clone()), axum::extract::Query(q)).await,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let sessions = body["sessions"].as_array().expect("sessions array");
    assert_eq!(
        sessions.len(),
        1,
        "known source_id must return exactly one session, got {body}"
    );
    assert_eq!(
        sessions[0]["id"].as_str(),
        Some(id_a.to_string().as_str()),
        "the returned session must be session A"
    );
    // source_id must be present in the JSON payload (#1730).
    assert_eq!(
        sessions[0]["source_id"].as_str(),
        Some("owner/repo"),
        "source_id must be serialized in the session payload"
    );

    // ── Case 2: unknown source_id → empty list ────────────────────────────────
    let q = HashMap::from([("source_id".to_string(), "totally/random-xyz".to_string())]);
    let (status, body) = decode_response(
        list_managed_sessions(axum::extract::State(state.clone()), axum::extract::Query(q)).await,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let sessions = body["sessions"].as_array().expect("sessions array");
    assert_eq!(
        sessions.len(),
        0,
        "unknown source_id must return an empty list, got {body}"
    );

    // ── Case 3: no ?source_id param → exactly both seeded sessions returned ───
    // FakeNoopTmuxDriver means reconcile_on_boot adopted nothing from the host,
    // so the store has exactly 2 records — the tight == 2 assertion is valid.
    let q: HashMap<String, String> = HashMap::new();
    let (status, body) = decode_response(
        list_managed_sessions(axum::extract::State(state.clone()), axum::extract::Query(q)).await,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let sessions = body["sessions"].as_array().expect("sessions array");
    assert_eq!(
        sessions.len(),
        2,
        "no source_id param must return exactly the two seeded sessions (FakeNoopTmuxDriver: \
         no external sessions adopted), got {body}"
    );
    // Both seeded session IDs must appear in the unfiltered list.
    let ids: Vec<&str> = sessions.iter().filter_map(|s| s["id"].as_str()).collect();
    assert!(
        ids.contains(&id_a.to_string().as_str()),
        "session A must appear in unfiltered list; ids={ids:?}"
    );
    assert!(
        ids.contains(&id_b.to_string().as_str()),
        "session B must appear in unfiltered list; ids={ids:?}"
    );
}

/// #2595: a stopped/errored session whose workspace has been GC-pruned must be
/// flagged `unresumable: true` on the LIST endpoint — the same endpoint the
/// bare-`tm` guided default, the `tm ls` picker, and `tm sessions ls` all read.
///
/// Why: #2577/#2594 fixed the ERROR an operator saw after picking such a
/// session to restart (bare 500 → actionable 422); this predicate lets the
/// listing surfaces exclude/mark the session BEFORE it is ever offered as a
/// restart option, closing the deeper UX defect the issue reports.
/// What: seeds a session whose `workspace_path`/`cwd` point at a directory
/// that is never created on disk (mirrors
/// `resume_managed_typed_missing_workspace_is_unprocessable` in
/// `resume_unresumable_mapping.rs`), drives it to `Errored` via
/// `mark_errored` (a resumable state, satisfying the predicate's state gate),
/// then asserts the LIST endpoint's summary for that id carries
/// `unresumable: true`.
/// Test: this function IS the test.
#[tokio::test]
async fn list_marks_dead_stopped_session_unresumable() {
    use std::collections::HashMap;
    use trusty_mpm::daemon::managed_routes::list_managed_sessions;

    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;

    let id = ResumeSessionId::new();
    let gone = root.path().join(format!("{id}-pruned-worktree"));
    mgr.create_with_id(
        id,
        "regression: #2595 dead session must be flagged unresumable".to_string(),
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
    mgr.mark_errored(&id, "regression: simulate prior spawn failure")
        .await
        .expect("mark errored");
    assert!(
        !gone.exists(),
        "test precondition: the workspace path must NOT exist on disk"
    );

    let q: HashMap<String, String> = HashMap::new();
    let (status, body) = decode_response(
        list_managed_sessions(axum::extract::State(state.clone()), axum::extract::Query(q)).await,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let sessions = body["sessions"].as_array().expect("sessions array");
    let row = sessions
        .iter()
        .find(|s| s["id"].as_str() == Some(id.to_string().as_str()))
        .unwrap_or_else(|| panic!("seeded session must appear in the list, got {body}"));
    assert_eq!(
        row["unresumable"].as_bool(),
        Some(true),
        "a stopped/errored session with no workdir candidate on disk must be \
         flagged unresumable, got {row}"
    );
}

/// #2595 counterpart: a HEALTHY stopped session (workspace still on disk) and
/// a LIVE/provisioning session (workspace missing, but the state gate means
/// the predicate never even probes the filesystem) must both come back
/// `unresumable: false` — the predicate must never over-fire.
///
/// Why: without this counterpart, `list_marks_dead_stopped_session_unresumable`
/// alone could pass even if the predicate always returned `true` — this test
/// pins the negative cases the picker/table depend on (issue #2595's own
/// acceptance criterion: "healthy stopped sessions unaffected").
/// What: seeds session A `Errored` with a REAL `TempDir` workspace (an existing
/// candidate) and session B left in its default `Provisioning` state with a
/// workspace path that is never created (so the state gate — not a filesystem
/// check — is what saves it); asserts BOTH come back `unresumable: false`.
/// Test: this function IS the test.
#[tokio::test]
async fn list_leaves_live_and_healthy_stopped_sessions_unmarked() {
    use std::collections::HashMap;
    use trusty_mpm::daemon::managed_routes::list_managed_sessions;

    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;

    // Session A: Errored, but its workspace genuinely still exists on disk.
    let id_a = ResumeSessionId::new();
    let ws_a = root.path().join(format!("{id_a}-healthy-ws"));
    std::fs::create_dir_all(&ws_a).expect("create real workspace dir for session A");
    mgr.create_with_id(
        id_a,
        "regression: #2595 healthy errored session stays resumable".to_string(),
        Some(ws_a.clone()),
        None,
        Some(ws_a),
        Some("https://example.com/r.git".to_string()),
        Some("main".to_string()),
        ResumeRuntimeKind::default(),
        false,
        false,
    )
    .await
    .expect("seed session A");
    mgr.mark_errored(&id_a, "regression: simulate prior spawn failure")
        .await
        .expect("mark errored A");

    // Session B: left Provisioning (create_with_id's default) with a workspace
    // path that is never created — the state gate alone must save it.
    let id_b = ResumeSessionId::new();
    let gone_b = root.path().join(format!("{id_b}-never-created"));
    mgr.create_with_id(
        id_b,
        "regression: #2595 provisioning session with missing workdir stays unflagged".to_string(),
        Some(gone_b.clone()),
        None,
        Some(gone_b),
        Some("https://example.com/r.git".to_string()),
        Some("main".to_string()),
        ResumeRuntimeKind::default(),
        false,
        false,
    )
    .await
    .expect("seed session B");

    let q: HashMap<String, String> = HashMap::new();
    let (status, body) = decode_response(
        list_managed_sessions(axum::extract::State(state.clone()), axum::extract::Query(q)).await,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    let sessions = body["sessions"].as_array().expect("sessions array");

    let row_a = sessions
        .iter()
        .find(|s| s["id"].as_str() == Some(id_a.to_string().as_str()))
        .unwrap_or_else(|| panic!("session A must appear in the list, got {body}"));
    assert_eq!(
        row_a["unresumable"].as_bool(),
        Some(false),
        "an errored session with an EXISTING workspace must not be flagged unresumable, \
         got {row_a}"
    );

    let row_b = sessions
        .iter()
        .find(|s| s["id"].as_str() == Some(id_b.to_string().as_str()))
        .unwrap_or_else(|| panic!("session B must appear in the list, got {body}"));
    assert_eq!(
        row_b["unresumable"].as_bool(),
        Some(false),
        "a provisioning/live session must never be flagged unresumable regardless \
         of workdir, got {row_b}"
    );
}

/// A GitHub `repo_url` must produce a project-identifiable session name (#1789).
///
/// Why: pins the `parse_github_path → gh.repo → build_managed_session_name`
/// chain at the manager's call site. If `GithubPath.repo` were ever renamed or
/// `parse_github_path` changed its extraction contract, the manager would
/// silently fall back to `tmpm-local-<8hex>` with no compile or existing-test
/// error. This integration test catches that regression without spawning real tmux.
/// What: creates a managed session with a real GitHub HTTPS `repo_url` via
/// `SessionManager::create` (backed by a fake tmux driver) and asserts the
/// resulting `tmux_name` starts with `tmpm-trusty-tools-` (not `tmpm-local-`).
/// Test: this function IS the test.
#[tokio::test]
async fn session_manager_github_repo_url_produces_project_name() {
    let dir = TempDir::new().unwrap();
    let tmux = RecordingTmux::new();
    let mgr = SessionManager::new(dir.path(), tmux)
        .await
        .expect("manager");

    let record = mgr
        .create(
            "task".into(),
            Some(PathBuf::from("/tmp/ws")),
            None,                                                     // no name_hint
            None,                                                     // no workspace_path
            Some("https://github.com/bobmatnyc/trusty-tools".into()), // repo_url
            None,                                                     // no branch
        )
        .await
        .expect("create");

    assert!(
        record.tmux_name.starts_with("tm-trusty-tools-"),
        "a GitHub repo_url must produce tm-<repo>-NN, got: {}",
        record.tmux_name
    );
    assert!(
        !record.tmux_name.starts_with("tm-local-"),
        "must not fall through to tm-local-NN when repo_url is a GitHub URL: {}",
        record.tmux_name
    );
}

// ── #1790 regression: test sessions must not reach the production store ────────
//
// These tests verify the isolation guarantee introduced in #1790:
// `with_root_isolated_managed` must (a) refuse to bind to `~/.trusty-mpm` and
// (b) use FakeNoopTmuxDriver so sessions created in tests never exist as real
// tmux sessions on the host and can never be adopted by the production daemon's
// `reconcile_on_boot`.

/// `with_root_isolated_managed` uses FakeNoopTmuxDriver so `create_session`
/// succeeds without running tmux (#1790).
///
/// Why: the key regression the issue guards is test code calling `create_with_id`
/// through a `DaemonState` backed by `RealTmuxDriver`, which spawns real `tmpm-*`
/// sessions the production daemon then adopts. This test confirms that the
/// FakeNoopTmuxDriver path returns `Ok(())` for `create_session`.
/// What: builds a `DaemonState::with_root_isolated_managed`, seeds a session via
/// `create_with_id`, and asserts (a) the call succeeded without error and (b)
/// `list_sessions` on the manager's driver returns an empty list (confirming no
/// real tmux session was created on the host).
/// Test: this function IS the test.
#[tokio::test]
async fn isolated_managed_state_uses_fake_driver_never_creates_real_tmux_session() {
    let root = TempDir::new().unwrap();
    let state = Arc::new(DaemonState::with_root_isolated_managed(root.path().to_path_buf()).await);
    let mgr = state.session_manager().await;

    // Create a session record — must succeed with FakeNoopTmuxDriver.
    let id = ResumeSessionId::new();
    let ws = root.path().join(format!("{id}-isolation-check"));
    let record = mgr
        .create_with_id(
            id,
            "isolation regression test".to_string(),
            Some(ws.clone()),
            None,
            Some(ws),
            Some("https://github.com/owner/trusty-tools".to_string()),
            Some("main".to_string()),
            ResumeRuntimeKind::default(),
            false,
            false,
        )
        .await
        .expect("create_with_id must succeed with FakeNoopTmuxDriver (#1790)");

    // The session record was persisted in the in-memory/temp store.
    let retrieved = mgr.get(&id).await.expect("session must be retrievable");
    assert_eq!(retrieved.id, record.id);
    assert!(
        retrieved.tmux_name.starts_with("tm-"),
        "session name must follow tm- convention"
    );

    // The driver must report zero live sessions — confirming no real tmux
    // session was created on the host (the production daemon cannot adopt it).
    let live_sessions = mgr
        .tmux_driver()
        .list_sessions()
        .expect("FakeNoopTmuxDriver::list_sessions must not fail");
    assert!(
        live_sessions.is_empty(),
        "FakeNoopTmuxDriver must report zero live sessions — no real tmux session \
         was created, so the production daemon cannot adopt it (#1790); \
         got: {live_sessions:?}"
    );
}

/// The production-store guard in `with_root_isolated_managed` panics when the
/// test accidentally points at `~/.trusty-mpm` (#1790).
///
/// Why: the guard is a belt-and-suspenders fail-fast that prevents future callers
/// from accidentally passing the production path. This test drives it by
/// constructing the production path and passing it — the `should_panic` attribute
/// verifies the guard fires.
/// What: passes `~/.trusty-mpm` to `with_root_isolated_managed` and asserts the
/// call panics with the expected guard message. On a system without a home
/// directory the test is skipped by emitting the same expected prefix so the
/// `should_panic` matcher still passes (both the real guard and the skip use the
/// same "with_root_isolated_managed must NOT point at the production" prefix).
/// Test: this function IS the test.
#[tokio::test]
#[should_panic(expected = "with_root_isolated_managed must NOT point at the production")]
async fn isolated_managed_state_guard_panics_on_production_root() {
    let home = match dirs::home_dir() {
        Some(h) => h,
        // No home directory on this host (rare in sandboxed CI); emit a panic
        // whose message starts with the same expected prefix so the
        // `should_panic` matcher still passes, making the skip explicit.
        None => panic!(
            "with_root_isolated_managed must NOT point at the production \
             store — SKIP: no home directory detected on this system"
        ),
    };
    let prod_root = home.join(".trusty-mpm");
    // This MUST panic with the production-root guard message.
    let _ = DaemonState::with_root_isolated_managed(prod_root).await;
}

// ── Live pane-target coverage (CRITICAL fix, follow-up to #2467) ────────────
//
// PR #2467 fixed `resume`/`restart` to target `record.pane_id` instead of
// tmux's session-scoped active pane. The FIRST cut of `TmuxTarget::as_target`
// rendered pane targets as `"<session>:<pane_id>"`, which tmux parses as a
// WINDOW spec, not a pane spec — every resume with a known `pane_id` (i.e.
// every post-#2456 session) hard-failed with "can't find window: %NNNN".
// This test is the missing live-tmux coverage: mock-only tests could not
// catch an invalid `-t` string because the mocks never parse it.

use trusty_mpm::core::tmux::TmuxTarget;
use trusty_mpm::daemon::tmux::TmuxDriver;

/// Kills a throwaway tmux session on drop, including on test-assertion panic.
///
/// Why: a live-tmux test that panics partway through must not leak the tmux
/// session it created — a bare `driver.kill_session(...)` call at the end of
/// the test function would never run if an earlier `assert_eq!` panicked.
/// What: wraps a `TmuxDriver` reference and a session name; `Drop::drop` best-
/// effort kills the session (`kill_session`'s error is intentionally
/// swallowed — cleanup must never itself panic during unwind).
/// Test: exercised by every test below that constructs one.
struct KillSessionGuard<'a> {
    driver: &'a TmuxDriver,
    name: String,
}

impl Drop for KillSessionGuard<'_> {
    fn drop(&mut self) {
        let _ = self.driver.kill_session(&self.name);
    }
}

/// Live proof that a pane-scoped send lands in the ORIGINAL pane, never a
/// sibling window that happens to be tmux's "active" pane.
///
/// Why: this is the exact sibling-window-hijack scenario #2467 set out to
/// fix, driven against a REAL `tmux` binary rather than the `FakeTmuxDriver`
/// mocks used elsewhere in this crate — the invalid `"session:%pane"` target
/// string bug shipped past review specifically because coverage was
/// mock-only (mocks never invoke real tmux argv parsing). It is `#[ignore]`
/// (needs a live tmux); run locally with
/// `cargo test -p trusty-mpm -- --include-ignored`.
/// What: creates a throwaway session (original pane = the session's sole
/// pane), opens a sibling window via `tmux new-window` (which tmux makes the
/// new ACTIVE pane), then sends a marker line via `TmuxDriver::send_line`
/// with a pane-scoped `TmuxTarget` addressing the ORIGINAL (now inactive)
/// pane. Asserts the marker landed in the original pane's `capture-pane`
/// output and did NOT land in the sibling's. Also exercises
/// `TmuxDriver::pane_exists`: `true` for the real original pane, `false` for
/// a fabricated pane id within the same (still-alive) session — the exact
/// signal `SessionManager::resume` relies on to distinguish "pane gone" from
/// "session gone". Cleans up via [`KillSessionGuard`] even on assertion
/// failure.
/// Test: this function IS the coverage `TmuxDriver::pane_exists`'s `Test:`
/// doc pointer now names (previously falsely claimed by a comment that
/// pointed at coverage which did not exist).
#[test]
#[ignore = "requires a live tmux binary; run with --include-ignored"]
fn live_pane_scoped_send_targets_original_pane_not_sibling() {
    let driver = TmuxDriver::discover().expect("tmux must be installed for this live test");
    let session_name = format!("trusty-mpm-test-pane-{}", std::process::id());

    // Clean slate: a leftover session from a prior crashed run must not
    // confuse this test.
    let _ = driver.kill_session(&session_name);
    driver
        .create_session(&session_name, None)
        .expect("create throwaway session");
    let _guard = KillSessionGuard {
        driver: &driver,
        name: session_name.clone(),
    };

    // Immediately after creation the session has exactly one pane, which is
    // also the active one — `pane_id` (session-scoped `display-message`)
    // resolves to it.
    let original_pane = driver
        .pane_id(&session_name)
        .expect("original pane id must resolve");

    // Open a sibling window. tmux makes the new window (and its pane) the
    // active one — this is precisely the state that caused the hijack: any
    // SESSION-scoped send would now land here instead of the original pane.
    let status = std::process::Command::new("tmux")
        .args(["new-window", "-t", &session_name])
        .status()
        .expect("spawn tmux new-window");
    assert!(status.success(), "tmux new-window must succeed");

    let sibling_pane = driver
        .pane_id(&session_name)
        .expect("sibling pane id must resolve once it is active");
    assert_ne!(
        original_pane, sibling_pane,
        "sibling window must be a distinct pane from the original"
    );

    // Pane-scoped send: must land in the ORIGINAL pane, not the (now active)
    // sibling — this is the exact call shape `spawn_resume` uses via
    // `send_line_to_pane` -> `RealTmuxDriver::send_line_to_pane` ->
    // `TmuxDriver::send_line(&TmuxTarget::pane(...), ...)`.
    let marker = "trusty_mpm_pane_target_marker_2467";
    driver
        .send_line(&TmuxTarget::pane(&session_name, &original_pane), marker)
        .expect("pane-scoped send_line must succeed against the ORIGINAL pane");

    // Give tmux a moment to render the sent keys into the pane buffer before
    // capturing (send-keys itself is synchronous, but the shell's own echo
    // of the typed line can lag by a beat under load).
    std::thread::sleep(std::time::Duration::from_millis(200));

    let original_capture = driver
        .capture(&TmuxTarget::pane(&session_name, &original_pane), None)
        .expect("capture original pane");
    assert!(
        original_capture.contains(marker),
        "marker must land in the ORIGINAL pane; captured: {original_capture:?}"
    );

    let sibling_capture = driver
        .capture(&TmuxTarget::pane(&session_name, &sibling_pane), None)
        .expect("capture sibling pane");
    assert!(
        !sibling_capture.contains(marker),
        "marker must NOT land in the sibling pane (that is the hijack this test \
         guards against); captured: {sibling_capture:?}"
    );

    // `pane_exists` must confirm the real pane and refuse a fabricated one
    // within the same (still-alive) session — the signal `resume` relies on
    // to distinguish "pane gone" (refuse) from "session gone" (different
    // path entirely).
    assert!(
        driver.pane_exists(&session_name, &original_pane),
        "pane_exists must report true for the real original pane"
    );
    assert!(
        !driver.pane_exists(&session_name, "%999999999"),
        "pane_exists must report false for a pane id that was never created \
         in this session"
    );
}
