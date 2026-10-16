//! Tests for Bash-fed worktree recording (#8994 S2b).
//!
//! Every command here is first run for real through `sh -c` in the same `cwd`
//! the hook payload names, so the ledger is checked against what git did.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::core::doctor::CheckStatus;
use crate::core::worktree_ledger::fold::fold;
use crate::core::worktree_ledger::hook_ingest::read_ingest_failure;
use crate::core::worktree_ledger::{EventKind, LEDGER_FILE, Origin, WorktreeLedger, ledger_key};
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
        let Ok(read) = self.ledger().read() else {
            return 0;
        };
        read.events.iter().filter(|e| pred(&e.kind)).count()
    }

    fn live(&self) -> Vec<PathBuf> {
        let events = self.ledger().read().expect("read").events;
        fold(&events).live.into_keys().collect()
    }

    async fn bash(&self, event: HookEvent, command: &str, cwd: &Path) -> Option<CommandReport> {
        let handle = spawn_for_bash(&self.state, SessionId::new(), event, &bash(command, cwd))?;
        Some(handle.await.expect("ingest task"))
    }

    /// Run `command` in `cwd` for real, then deliver its `PostToolUse`.
    async fn run(&self, command: &str, cwd: &Path) -> Option<CommandReport> {
        let ok = sh(command, cwd);
        assert!(ok, "`{command}` must succeed in {}", cwd.display());
        self.bash(HookEvent::PostToolUse, command, cwd).await
    }
}

/// The payload `tm hook` forwards for a Bash `PostToolUse`.
fn bash(command: &str, cwd: &Path) -> Value {
    json!({
        "tool": "Bash",
        "cwd": cwd.display().to_string(),
        "input": { "command": command, "description": "x" },
    })
}

/// Run `command` through `sh -c` in `cwd`; `true` on exit 0.
fn sh(command: &str, cwd: &Path) -> bool {
    Command::new("sh")
        .args(["-c", command])
        .current_dir(cwd)
        .output()
        .expect("sh")
        .status
        .success()
}

/// The parent of the fixture's checkout, where sibling trees go.
fn root_of(fixture: &GitWorktreeFixture) -> PathBuf {
    fixture.repo.parent().expect("repo parent").to_path_buf()
}

fn observed_git_command(h: &Harness, tree: &Path) -> usize {
    let key = ledger_key(tree);
    let events = h.ledger().read().map(|r| r.events).unwrap_or_default();
    events
        .iter()
        .filter(|e| {
            e.path == key
                && matches!(
                    &e.kind,
                    EventKind::Observed {
                        origin: Origin::GitCommand,
                        ..
                    }
                )
        })
        .count()
}

/// Criterion 1: each real form records exactly one entry at the absolute path,
/// and the same event delivered again appends nothing.
#[tokio::test]
async fn each_worktree_add_form_records_one_entry() {
    let fixture = GitWorktreeFixture::new();
    let root = root_of(&fixture);
    let elsewhere = tempfile::tempdir().expect("tempdir");
    let h = Harness::new(&fixture.repo).await;
    let repo = fixture.repo.display().to_string();
    let forms: [(String, PathBuf, PathBuf); 4] = [
        (
            "git worktree add -b s2b/one ../wt-one".into(),
            fixture.repo.clone(),
            root.join("wt-one"),
        ),
        (
            format!("git -C {repo} worktree add -b s2b/two ../wt-two"),
            elsewhere.path().to_path_buf(),
            root.join("wt-two"),
        ),
        (
            format!("cd {repo} && git worktree add -b s2b/three .worktrees/three"),
            root.clone(),
            fixture.repo.join(".worktrees/three"),
        ),
        (
            "cd repo && git worktree add --detach ../wt-four".into(),
            root.clone(),
            root.join("wt-four"),
        ),
    ];
    for (command, cwd, tree) in &forms {
        let report = h.run(command, cwd).await.expect("a real add spawns work");
        assert_eq!(report.errors, Vec::<String>::new(), "{command}");
        assert!(
            matches!(report.added.as_slice(), [ObserveOutcome::Recorded { .. }]),
            "{command}: {report:?}"
        );
        assert_eq!(observed_git_command(&h, tree), 1, "{command}");
        // Replayed (a duplicate delivery): the ledger dedups.
        let again = h
            .bash(HookEvent::PostToolUse, command, cwd)
            .await
            .expect("work");
        assert_eq!(again.added, vec![ObserveOutcome::AlreadyLive], "{command}");
        assert_eq!(observed_git_command(&h, tree), 1, "{command}");
    }
    assert_eq!(h.live().len(), 4);
    let branch = fold(&h.ledger().read().unwrap().events).live[&ledger_key(&forms[0].2)]
        .branch
        .clone();
    assert_eq!(branch.as_deref(), Some("s2b/one"));
}

/// Criterion 2: a `git worktree add` that failed records nothing — no
/// directory, a directory git does not list, or a failure event.
#[tokio::test]
async fn a_failed_worktree_add_records_nothing() {
    let fixture = GitWorktreeFixture::new();
    let root = root_of(&fixture);
    let h = Harness::new(&fixture.repo).await;
    // `main` is checked out in the repo, so git refuses and creates nothing.
    let refused = "git worktree add ../wt-refused main";
    assert!(!sh(refused, &fixture.repo), "git must refuse");
    let report = h
        .bash(HookEvent::PostToolUse, refused, &fixture.repo)
        .await
        .expect("work");
    assert_eq!(report.added, vec![ObserveOutcome::NotADirectory]);
    // A non-empty directory already there: git refuses and leaves it.
    std::fs::create_dir_all(root.join("occupied")).unwrap();
    std::fs::write(root.join("occupied/file"), b"x").unwrap();
    let occupied = "git worktree add -b s2b/occ ../occupied";
    assert!(!sh(occupied, &fixture.repo), "git must refuse");
    let report = h
        .bash(HookEvent::PostToolUse, occupied, &fixture.repo)
        .await
        .expect("work");
    assert_eq!(report.added, vec![ObserveOutcome::NotListed]);
    // A failed tool call is not a `PostToolUse`: nothing is spawned.
    assert!(
        h.bash(HookEvent::PostToolUseFailure, occupied, &fixture.repo)
            .await
            .is_none()
    );
    assert_eq!(h.count(|_| true), 0, "nothing appended");
    // Control: the same harness records a real add.
    h.run("git worktree add -b s2b/ok ../wt-ok", &fixture.repo)
        .await
        .expect("work");
    assert_eq!(observed_git_command(&h, &root.join("wt-ok")), 1);
}

/// Criterion 3: `remove` and `prune` record `removed` only for a tree gone
/// from disk AND from `git worktree list`.
#[tokio::test]
async fn remove_and_prune_record_removed_only_when_gone_from_disk_and_git() {
    let fixture = GitWorktreeFixture::new();
    let root = root_of(&fixture);
    let h = Harness::new(&fixture.repo).await;
    for name in ["a", "b", "c"] {
        let command = format!("git worktree add -b s2b/{name} ../wt-{name}");
        h.run(&command, &fixture.repo).await.expect("work");
    }
    assert_eq!(h.live().len(), 3);
    // A: a real `git worktree remove`.
    let report = h
        .run("git worktree remove ../wt-a", &fixture.repo)
        .await
        .expect("work");
    assert_eq!(report.reconciled.iter().map(|r| r.removed).sum::<usize>(), 1);
    // B: deleted from disk only; git still lists it, so it stays live.
    std::fs::remove_dir_all(root.join("wt-b")).unwrap();
    let report = h
        .bash(HookEvent::PostToolUse, "git worktree remove ../wt-b", &fixture.repo)
        .await
        .expect("work");
    assert_eq!(report.reconciled.iter().map(|r| r.removed).sum::<usize>(), 0);
    assert_eq!(report.reconciled.iter().map(|r| r.still_listed).sum::<usize>(), 1);
    // C: a remove that did not happen (the tree is still on disk).
    let report = h
        .bash(HookEvent::PostToolUse, "git worktree remove ../wt-c", &fixture.repo)
        .await
        .expect("work");
    assert_eq!(report.reconciled.iter().map(|r| r.removed).sum::<usize>(), 0);
    // A dry-run prune runs no git worktree change and records nothing.
    assert!(
        h.bash(HookEvent::PostToolUse, "git worktree prune -n", &fixture.repo)
            .await
            .is_none()
    );
    // A real prune drops B from git; now B is removed. C stays.
    let report = h.run("git worktree prune", &fixture.repo).await.expect("work");
    assert_eq!(report.reconciled.iter().map(|r| r.removed).sum::<usize>(), 1);
    assert_eq!(h.count(|k| *k == EventKind::Removed), 2);
    assert_eq!(h.live(), vec![ledger_key(&root.join("wt-c"))]);
    assert!(root.join("wt-c").is_dir(), "nothing is deleted");
}

/// Criterion 4: a command that only mentions the words records nothing, even
/// when the path it mentions is a real worktree.
#[tokio::test]
async fn commands_that_only_mention_the_words_record_nothing() {
    let fixture = GitWorktreeFixture::new();
    let root = root_of(&fixture);
    let h = Harness::new(&fixture.repo).await;
    let tree = fixture.add_worktree_at(&root, "mentioned");
    let t = tree.display().to_string();
    let mentions = [
        format!("echo \"git worktree add {t}\""),
        format!("grep -rn 'git worktree add {t}' . || true"),
        format!("cat <<'EOF' > /dev/null\ngit worktree add {t}\nEOF"),
        format!("true # git worktree add {t}"),
        format!("# git worktree add {t}"),
    ];
    for command in &mentions {
        assert!(sh(command, &fixture.repo), "{command}");
        assert!(
            h.bash(HookEvent::PostToolUse, command, &fixture.repo)
                .await
                .is_none(),
            "{command}"
        );
    }
    assert_eq!(h.count(|_| true), 0, "nothing appended");
    // Control: the real call for the same path is recorded.
    let report = h
        .bash(HookEvent::PostToolUse, &format!("git worktree add {t}"), &fixture.repo)
        .await
        .expect("work");
    assert!(matches!(report.added.as_slice(), [ObserveOutcome::Recorded { .. }]));
}

/// Poll the sidecar written by a detached task, bounded.
async fn sidecar(ledger: &WorktreeLedger) -> Option<String> {
    for _ in 0..500 {
        if let Ok(Some(f)) = read_ingest_failure(ledger) {
            return Some(f.error);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    None
}

/// Criterion 6: a failed record never changes the verdict, reaches the sidecar
/// and the doctor row, and an unrelated S2a success does not clear it.
#[tokio::test]
async fn a_failed_record_is_never_silent_and_never_blocks() {
    let fixture = GitWorktreeFixture::new();
    let h = Harness::new(&fixture.repo).await;
    let command = "git worktree add -b s2b/unw ../wt-unwritable";
    assert!(sh(command, &fixture.repo));
    std::fs::create_dir_all(h.ledger().path()).unwrap();
    let svc = HookService::new(Arc::clone(&h.state));
    let verdict = svc.process(
        SessionId::new(),
        HookEvent::PostToolUse,
        bash(command, &fixture.repo),
    );
    assert_eq!(verdict, HookDecision::Allow);
    let error = sidecar(&h.ledger())
        .await
        .expect("a failed ledger write is recorded, never swallowed");
    assert!(error.contains("git worktree add"), "{error}");
    let row = || {
        crate::daemon::doctor::doctor_worktree_registry::check_worktree_registry(h.home.path())
    };
    assert_ne!(row().status, CheckStatus::Ok, "{}", row().message);
    // The ledger is writable again; an S2a success must not clear the alarm.
    std::fs::remove_dir(h.ledger().path()).unwrap();
    let agent = fixture.add_worktree_at(&fixture.repo.join(".claude/worktrees"), "agent-ok");
    let s2a = crate::daemon::services::worktree_ledger_ingest::spawn_for_event(
        &h.state,
        SessionId::new(),
        HookEvent::PreToolUse,
        &json!({ "cwd": agent.display().to_string() }),
    )
    .expect("an S2a observe")
    .await
    .expect("task");
    assert!(matches!(s2a.observed, Some(ObserveOutcome::Recorded { .. })));
    assert!(
        read_ingest_failure(&h.ledger()).unwrap().is_some(),
        "an unrelated success must not clear the S2b alarm"
    );
    assert_ne!(row().status, CheckStatus::Ok);
    // A command that cannot be parsed is reported the same way.
    let report = h
        .bash(HookEvent::PostToolUse, "git worktree add \"$WT\"", &fixture.repo)
        .await
        .expect("a parse failure spawns its report");
    assert_eq!(report.errors.len(), 1, "{report:?}");
    let failures = read_ingest_failure(&h.ledger()).unwrap().map(|f| f.failures);
    assert_eq!(failures, Some(2));
}
