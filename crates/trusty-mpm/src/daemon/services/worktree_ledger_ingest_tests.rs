//! Tests for the daemon's hook-fed worktree-ledger ingest (#8994 S2a).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::core::doctor::CheckStatus;
use crate::core::worktree_ledger::EventKind;
use crate::core::worktree_ledger::fold::fold;
use crate::core::worktree_ledger::hook_ingest::read_ingest_failure;
use crate::daemon::services::hook_service::{HookDecision, HookService};
use crate::session_manager::worktree_git_fixture::GitWorktreeFixture;

/// A daemon rooted at `<home>/.trusty-mpm` with `repo` registered.
struct Harness {
    home: tempfile::TempDir,
    state: Arc<DaemonState>,
}

impl Harness {
    async fn new(repo: &Path) -> Self {
        let home = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(home.path())
            .expect("canonical home")
            .join(".trusty-mpm");
        let registry =
            crate::project::ProjectRegistry::load(&crate::project::registry_data_dir_under(&root))
                .await
                .expect("registry");
        let project = serde_json::from_value(json!({
            "name": "repo",
            "repo_url": repo.display().to_string(),
            "default_branch": "main",
        }))
        .expect("project");
        registry.register(project).await.expect("register");
        let state = Arc::new(DaemonState::with_root(root));
        Self { home, state }
    }

    fn ledger(&self) -> WorktreeLedger {
        WorktreeLedger::at(self.state.framework_root().join(LEDGER_FILE))
    }

    fn count(&self, pred: impl Fn(&EventKind) -> bool) -> usize {
        let read = self.ledger().read().expect("read");
        read.events.iter().filter(|e| pred(&e.kind)).count()
    }

    async fn event(&self, event: HookEvent, payload: Value) -> Option<IngestReport> {
        let handle = spawn_for_event(&self.state, SessionId::new(), event, &payload)?;
        Some(handle.await.expect("ingest task"))
    }
}

fn agent_tree(fixture: &GitWorktreeFixture, name: &str) -> PathBuf {
    fixture.add_worktree_at(&fixture.repo.join(".claude").join("worktrees"), name)
}

fn tool_call(cwd: &Path) -> Value {
    json!({ "tool": "Bash", "agent_id": "a1", "cwd": cwd.display().to_string() })
}

#[tokio::test]
async fn repeated_events_record_a_tree_once() {
    let fixture = GitWorktreeFixture::new();
    let tree = agent_tree(&fixture, "agent-repeat");
    let h = Harness::new(&fixture.repo).await;
    let first = h
        .event(HookEvent::PreToolUse, tool_call(&tree.join("src")))
        .await
        .expect("first event names a new tree");
    assert_eq!(
        first.observed,
        Some(ObserveOutcome::Recorded {
            branch: Some("wt/agent-repeat".into())
        })
    );
    for event in [HookEvent::PreToolUse, HookEvent::PostToolUse] {
        assert!(
            h.event(event, tool_call(&tree)).await.is_none(),
            "{event:?}"
        );
    }
    // A restarted daemon has lost its in-memory claim; the ledger dedups.
    h.state.worktree_ingest().release(&tree);
    let again = h
        .event(HookEvent::PreToolUse, tool_call(&tree))
        .await
        .expect("the released tree is claimed again");
    assert_eq!(again.observed, Some(ObserveOutcome::AlreadyLive));
    assert_eq!(h.count(|k| matches!(k, EventKind::Observed { .. })), 1);
}

#[tokio::test]
async fn stop_marks_removed_only_after_the_tree_is_gone() {
    let fixture = GitWorktreeFixture::new();
    let tree = agent_tree(&fixture, "agent-stop");
    let h = Harness::new(&fixture.repo).await;
    h.event(HookEvent::PreToolUse, tool_call(&tree)).await;
    // While the agent runs, a stop from another agent removes nothing.
    let running = h
        .event(HookEvent::SubagentStop, json!({ "agent_id": "other" }))
        .await
        .expect("a stop queues a reconcile");
    assert_eq!(running.reconciled.map(|r| r.removed), Some(0));
    // The harness deletes the tree and its git registration.
    let removed = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&tree)
        .current_dir(&fixture.repo)
        .status()
        .expect("git");
    assert!(removed.success());
    // Out of order: a late tool event for the gone tree records nothing.
    assert!(
        h.event(HookEvent::PostToolUse, tool_call(&tree))
            .await
            .is_none()
    );
    let ended = h
        .event(HookEvent::SessionEnd, json!({}))
        .await
        .expect("session end queues a reconcile");
    assert_eq!(ended.reconciled.map(|r| r.removed), Some(1));
    assert!(fold(&h.ledger().read().unwrap().events).live.is_empty());
    assert_eq!(h.count(|k| *k == EventKind::Removed), 1);
}

#[tokio::test]
async fn odd_payloads_never_panic_and_record_nothing() {
    let fixture = GitWorktreeFixture::new();
    let h = Harness::new(&fixture.repo).await;
    let outside = tempfile::tempdir().unwrap();
    let unregistered = outside.path().join(".claude/worktrees/agent-x");
    std::fs::create_dir_all(&unregistered).unwrap();
    let payloads = [
        json!({}),
        json!({ "cwd": null }),
        json!({ "cwd": 42 }),
        json!({ "cwd": "" }),
        json!({ "cwd": "relative/.claude/worktrees/agent-a" }),
        json!({ "cwd": "/no/such/.claude/worktrees/agent-a" }),
        json!({ "cwd": format!("{}/.claude/worktrees/agent-a/../../..", fixture.repo.display()) }),
        json!({ "cwd": "/x/.claude/worktrees/agent-\u{0}nul" }),
        json!({ "cwd": unregistered.display().to_string() }),
        json!("not an object"),
    ];
    for payload in payloads {
        if let Some(report) = h.event(HookEvent::PreToolUse, payload.clone()).await {
            assert!(
                matches!(
                    report.observed,
                    Some(ObserveOutcome::NotADirectory | ObserveOutcome::Unregistered)
                ),
                "{payload}: {report:?}"
            );
            assert_eq!(report.error, None, "{payload}");
        }
    }
    assert!(!h.ledger().read().unwrap().present, "nothing appended");
}

/// The ledger path is a directory, so every append fails. The hook verdict is
/// still `Allow`, and the failure reaches the sidecar and the doctor row.
#[tokio::test]
async fn an_unwritable_ledger_leaves_the_verdict_and_reaches_the_doctor_row() {
    let fixture = GitWorktreeFixture::new();
    let tree = agent_tree(&fixture, "agent-unwritable");
    let h = Harness::new(&fixture.repo).await;
    std::fs::create_dir_all(h.ledger().path()).unwrap();
    let svc = HookService::new(Arc::clone(&h.state));
    let verdict = svc.process(SessionId::new(), HookEvent::PreToolUse, tool_call(&tree));
    assert_eq!(verdict, HookDecision::Allow);
    // The ingest is detached from `process`; poll for its sidecar, bounded.
    let ledger = h.ledger();
    let mut failure = None;
    for _ in 0..500 {
        failure = read_ingest_failure(&ledger).expect("readable sidecar");
        if failure.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let failure = failure.expect("a failed ledger write is recorded, never swallowed");
    assert!(failure.error.contains("observe"), "{failure:?}");
    let row =
        crate::daemon::doctor::doctor_worktree_registry::check_worktree_registry(h.home.path());
    assert_ne!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(
        row.message.contains("hook ingest failed"),
        "{}",
        row.message
    );
    // The same failure through a stop event is reported in the task result.
    let stopped = h
        .event(HookEvent::SubagentStop, json!({}))
        .await
        .expect("reconcile queued");
    assert!(stopped.error.is_some(), "{stopped:?}");
    // Observe (1), then the stop's retry of that pending tree (2) and its
    // reconcile (3) — #8994 retries a failed tree once per stop event.
    assert_eq!(
        read_ingest_failure(&ledger).unwrap().map(|f| f.failures),
        Some(3)
    );
}

/// Make the ledger file read-only: reads succeed, every append fails.
fn make_read_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444)).unwrap();
}

fn make_writable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

/// #8994: the append-error arm — the ledger file exists and reads, but the
/// append fails. The verdict is still `Allow` and the sidecar is written.
#[tokio::test]
async fn a_read_only_ledger_file_reaches_the_sidecar() {
    let fixture = GitWorktreeFixture::new();
    let first = agent_tree(&fixture, "agent-first");
    let second = agent_tree(&fixture, "agent-second");
    let h = Harness::new(&fixture.repo).await;
    h.event(HookEvent::PreToolUse, tool_call(&first)).await;
    assert!(h.ledger().read().unwrap().present, "the ledger file exists");
    make_read_only(h.ledger().path());
    let svc = HookService::new(Arc::clone(&h.state));
    let verdict = svc.process(SessionId::new(), HookEvent::PreToolUse, tool_call(&second));
    assert_eq!(verdict, HookDecision::Allow);
    let ledger = h.ledger();
    let mut failure = None;
    for _ in 0..500 {
        failure = read_ingest_failure(&ledger).expect("readable sidecar");
        if failure.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let failure = failure.expect("a failed append is recorded, never swallowed");
    assert!(failure.error.contains("observe"), "{failure:?}");
    make_writable(ledger.path());
}

/// #8994 HIGH 1: a stop event that also claims a new tree, with an unreadable
/// project registry, must not leave `reconcile_queued` set. The next
/// `SessionEnd` still writes `removed` for a tree that is gone.
#[tokio::test]
async fn a_failed_registry_read_does_not_absorb_later_stops() {
    let fixture = GitWorktreeFixture::new();
    let gone = agent_tree(&fixture, "agent-gone");
    let fresh = agent_tree(&fixture, "agent-fresh");
    let h = Harness::new(&fixture.repo).await;
    h.event(HookEvent::PreToolUse, tool_call(&gone)).await;
    let registry = crate::project::registry_data_dir_under(h.state.framework_root());
    std::fs::write(registry.join("projects.json"), b"{ not json").unwrap();
    let failed = h
        .event(HookEvent::SubagentStop, tool_call(&fresh))
        .await
        .expect("a stop naming a new tree queues work");
    let message = failed.error.expect("the registry failure is reported");
    assert!(message.contains("project registry unreadable"), "{message}");
    // The harness removes `gone`; the registry is still unreadable.
    let removed = std::process::Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(&gone)
        .current_dir(&fixture.repo)
        .status()
        .expect("git");
    assert!(removed.success());
    let ended = h
        .event(HookEvent::SessionEnd, json!({}))
        .await
        .expect("the failed task did not leave the reconcile queued");
    assert_eq!(ended.reconciled.map(|r| r.removed), Some(1));
    assert_eq!(h.count(|k| *k == EventKind::Removed), 1);
    // Not silent: the sidecar records it and the doctor row leaves Ok.
    assert!(read_ingest_failure(&h.ledger()).unwrap().is_some());
    let row =
        crate::daemon::doctor::doctor_worktree_registry::check_worktree_registry(h.home.path());
    assert_ne!(row.status, CheckStatus::Ok, "{}", row.message);
}

/// #8994 HIGH 2: a tree whose `observed` write failed is retried at the next
/// stop, and an unrelated success does not clear the sidecar or the doctor Warn.
#[tokio::test]
async fn a_failed_tree_is_retried_and_the_alarm_outlives_an_unrelated_success() {
    let fixture = GitWorktreeFixture::new();
    let seed = agent_tree(&fixture, "agent-seed");
    let a = agent_tree(&fixture, "agent-a");
    let b = agent_tree(&fixture, "agent-b");
    let h = Harness::new(&fixture.repo).await;
    h.event(HookEvent::PreToolUse, tool_call(&seed)).await;
    let ledger = h.ledger();
    let warn =
        || crate::daemon::doctor::doctor_worktree_registry::check_worktree_registry(h.home.path());
    // Tree A: the append fails.
    make_read_only(ledger.path());
    let failed = h.event(HookEvent::PreToolUse, tool_call(&a)).await.unwrap();
    assert!(failed.error.is_some(), "{failed:?}");
    assert!(read_ingest_failure(&ledger).unwrap().is_some());
    // Tree B succeeds; A is still missing, so the alarm stays.
    make_writable(ledger.path());
    let ok = h.event(HookEvent::PreToolUse, tool_call(&b)).await.unwrap();
    assert!(matches!(ok.observed, Some(ObserveOutcome::Recorded { .. })));
    assert!(
        read_ingest_failure(&ledger).unwrap().is_some(),
        "an unrelated success must not clear the alarm for tree A"
    );
    assert_ne!(warn().status, CheckStatus::Ok);
    // The next stop retries A; with nothing pending the alarm clears.
    let stopped = h.event(HookEvent::SubagentStop, json!({})).await.unwrap();
    assert_eq!(stopped.error, None, "{stopped:?}");
    assert!(read_ingest_failure(&ledger).unwrap().is_none());
    assert_eq!(warn().status, CheckStatus::Ok);
    let live = fold(&ledger.read().unwrap().events).live;
    assert!(live.contains_key(&crate::core::worktree_ledger::ledger_key(&a)));
    assert_eq!(h.count(|k| matches!(k, EventKind::Observed { .. })), 3);
}
