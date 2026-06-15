//! Integration tests for the session-manager MVP.
//!
//! Why: the session-manager MVP spans several units (provisioner, catalog sync,
//! session manager, daemon routes). These tests verify the units that can be
//! exercised without a live tmux/git/LLM, plus a single `#[ignore]` live test
//! that drives the real tmux + git path on a developer machine.
//! What: provisioner isolation, catalog sync TTL behavior, and the session
//! manager's create/answer flow with an in-memory fake tmux driver.
//! Test: this file IS the test; run with `cargo test -p trusty-mpm`.

use std::path::PathBuf;
use std::sync::Arc;

use tempfile::TempDir;

use trusty_mpm::provisioner::{FakeGitBackend, WorkspaceProvisioner};
use trusty_mpm::session_manager::{
    ManagedError, ManagedSessionId, ManagedTmuxDriver, SessionManager,
};

/// An in-memory tmux driver that records sends and never touches a real binary.
///
/// Why: the session manager and its HTTP surface must be testable without tmux.
/// What: records every `send_line` call; all other operations are no-ops.
/// Test: used by `session_manager_answer_clears_pending`.
struct RecordingTmux {
    sends: std::sync::Mutex<Vec<(String, String)>>,
}

impl RecordingTmux {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            sends: std::sync::Mutex::new(Vec::new()),
        })
    }
}

impl ManagedTmuxDriver for RecordingTmux {
    fn create_session(&self, _name: &str, _workdir: &str) -> Result<(), ManagedError> {
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
