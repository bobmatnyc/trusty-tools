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
    assert_eq!(
        report.reconciled.iter().map(|r| r.removed).sum::<usize>(),
        1
    );
    // B: deleted from disk only; git still lists it, so it stays live.
    std::fs::remove_dir_all(root.join("wt-b")).unwrap();
    let report = h
        .bash(
            HookEvent::PostToolUse,
            "git worktree remove ../wt-b",
            &fixture.repo,
        )
        .await
        .expect("work");
    assert_eq!(
        report.reconciled.iter().map(|r| r.removed).sum::<usize>(),
        0
    );
    assert_eq!(
        report
            .reconciled
            .iter()
            .map(|r| r.still_listed)
            .sum::<usize>(),
        1
    );
    // C: a remove that did not happen (the tree is still on disk).
    let report = h
        .bash(
            HookEvent::PostToolUse,
            "git worktree remove ../wt-c",
            &fixture.repo,
        )
        .await
        .expect("work");
    assert_eq!(
        report.reconciled.iter().map(|r| r.removed).sum::<usize>(),
        0
    );
    // A dry-run prune runs no git worktree change and records nothing.
    assert!(
        h.bash(
            HookEvent::PostToolUse,
            "git worktree prune -n",
            &fixture.repo
        )
        .await
        .is_none()
    );
    // A real prune drops B from git; now B is removed. C stays.
    let report = h
        .run("git worktree prune", &fixture.repo)
        .await
        .expect("work");
    assert_eq!(
        report.reconciled.iter().map(|r| r.removed).sum::<usize>(),
        1
    );
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
        .bash(
            HookEvent::PostToolUse,
            &format!("git worktree add {t}"),
            &fixture.repo,
        )
        .await
        .expect("work");
    assert!(matches!(
        report.added.as_slice(),
        [ObserveOutcome::Recorded { .. }]
    ));
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

/// Make the next ledger write fail, deliver one real add, and wait for the
/// sidecar; the ledger is writable again on return.
async fn fail_one_record(h: &Harness, fixture: &GitWorktreeFixture) {
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
    std::fs::remove_dir(h.ledger().path()).unwrap();
}

/// One S2a observe of a fresh agent-isolation tree, awaited.
async fn s2a_success(state: &Arc<DaemonState>, fixture: &GitWorktreeFixture, name: &str) {
    let agent = fixture.add_worktree_at(&fixture.repo.join(".claude/worktrees"), name);
    let s2a = crate::daemon::services::worktree_ledger_ingest::spawn_for_event(
        state,
        SessionId::new(),
        HookEvent::PreToolUse,
        &json!({ "cwd": agent.display().to_string() }),
    )
    .expect("an S2a observe")
    .await
    .expect("task");
    assert!(matches!(
        s2a.observed,
        Some(ObserveOutcome::Recorded { .. })
    ));
}

/// Criterion 6: a failed record never changes the verdict, reaches the sidecar
/// and the doctor row, and an unrelated S2a success does not clear it.
#[tokio::test]
async fn a_failed_record_is_never_silent_and_never_blocks() {
    let fixture = GitWorktreeFixture::new();
    let h = Harness::new(&fixture.repo).await;
    fail_one_record(&h, &fixture).await;
    let row =
        || crate::daemon::doctor::doctor_worktree_registry::check_worktree_registry(h.home.path());
    assert_ne!(row().status, CheckStatus::Ok, "{}", row().message);
    s2a_success(&h.state, &fixture, "agent-ok").await;
    assert!(
        read_ingest_failure(&h.ledger()).unwrap().is_some(),
        "an unrelated success must not clear the S2b alarm"
    );
    assert_ne!(row().status, CheckStatus::Ok);
    assert!(read_ingest_failure(&h.ledger()).unwrap().unwrap().sticky);
    // A command that cannot be parsed is reported the same way.
    let report = h
        .bash(
            HookEvent::PostToolUse,
            "git worktree add \"$WT\"",
            &fixture.repo,
        )
        .await
        .expect("a parse failure spawns its report");
    assert_eq!(report.errors.len(), 1, "{report:?}");
    let failures = read_ingest_failure(&h.ledger())
        .unwrap()
        .map(|f| f.failures);
    assert_eq!(failures, Some(2));
}

/// Criterion 6 across a restart: the S2b record is on disk, so a new daemon's
/// S2a success (nothing pending, in-memory flag gone) must not clear it.
#[tokio::test]
async fn an_s2b_failure_survives_a_restart_and_an_s2a_success() {
    let fixture = GitWorktreeFixture::new();
    let h = Harness::new(&fixture.repo).await;
    fail_one_record(&h, &fixture).await;
    let restarted = Arc::new(DaemonState::with_root(
        h.state.framework_root().to_path_buf(),
    ));
    s2a_success(&restarted, &fixture, "agent-after-restart").await;
    assert!(
        read_ingest_failure(&h.ledger()).unwrap().is_some(),
        "a restart plus an S2a success must not hide the missed tree"
    );
    let row =
        crate::daemon::doctor::doctor_worktree_registry::check_worktree_registry(h.home.path());
    assert_ne!(row.status, CheckStatus::Ok, "{}", row.message);
}

/// Criterion 2: a `git worktree add` git refused because the path was already
/// a registered worktree (made earlier, not yet in the ledger) records
/// nothing: the tree is left for the backfill, never owned by this session.
#[tokio::test]
async fn an_add_refused_for_an_existing_worktree_records_nothing() {
    let fixture = GitWorktreeFixture::new();
    let root = root_of(&fixture);
    let h = Harness::new(&fixture.repo).await;
    let tree = fixture.add_worktree_at(&root, "wt-pre");
    // Made an hour ago: its gitfile predates any Bash call that could be live.
    let gitfile = std::fs::OpenOptions::new()
        .write(true)
        .open(tree.join(".git"))
        .expect("gitfile");
    let hour = std::time::SystemTime::now() - Duration::from_secs(3600);
    gitfile.set_modified(hour).expect("backdate");
    let command = "git worktree add -b s2b/dup ../wt-pre";
    assert!(!sh(command, &fixture.repo), "git must refuse");
    let report = h
        .bash(HookEvent::PostToolUse, command, &fixture.repo)
        .await
        .expect("work");
    assert_eq!(report.added, vec![ObserveOutcome::NotNew]);
    assert_eq!(observed_git_command(&h, &tree), 0, "no owner is claimed");
    assert_eq!(h.count(|_| true), 0, "nothing appended");
    assert_eq!(report.errors, Vec::<String>::new());
}

/// A failure record with the sticky marker, as the warn path leaves it.
fn assert_sticky_failure(h: &Harness, report: &CommandReport, cause: &str) {
    assert_eq!(report.errors.len(), 1, "{report:?}");
    assert!(report.errors[0].contains("git worktree add"), "{report:?}");
    let failure = read_ingest_failure(&h.ledger())
        .unwrap()
        .expect("the warn path records the failure");
    assert!(failure.error.contains(cause), "{failure:?}");
    assert!(failure.sticky, "{failure:?}");
    assert_eq!(h.count(|_| true), 0, "nothing appended");
    let row =
        crate::daemon::doctor::doctor_worktree_registry::check_worktree_registry(h.home.path());
    assert_ne!(row.status, CheckStatus::Ok, "{}", row.message);
}

/// Fail-open, error arm `Git`: a git that gives no answer (here, a deadline
/// already past, so none is spawned and the call reads as a timeout) is
/// recorded sticky, never silently dropped as "not recorded".
#[tokio::test]
async fn a_git_that_gives_no_answer_is_recorded_sticky() {
    let fixture = GitWorktreeFixture::new();
    let h = Harness::new(&fixture.repo).await;
    let command = "git worktree add -b s2b/slow ../wt-slow";
    assert!(sh(command, &fixture.repo));
    let parsed = parse_worktree_command(command, Some(&fixture.repo));
    let checkouts = Ok(vec![fixture.repo.clone()]);
    let tracker = IngestTracker::default();
    let report = crate::core::bounded_proc::with_deadline(std::time::Instant::now(), || {
        ingest(&tracker, &h.ledger(), parsed, &checkouts, "s", None)
    });
    assert_sticky_failure(&h, &report, "gave no answer");
}

/// Fail-open, error arm "registry unreadable": the add cannot be judged, so it
/// is recorded sticky.
#[tokio::test]
async fn an_unreadable_project_registry_is_recorded_sticky() {
    let fixture = GitWorktreeFixture::new();
    let h = Harness::new(&fixture.repo).await;
    let registry = crate::project::registry_data_dir_under(h.state.framework_root());
    std::fs::write(registry.join("projects.json"), b"{torn").unwrap();
    let command = "git worktree add -b s2b/reg ../wt-reg";
    assert!(sh(command, &fixture.repo));
    let report = h
        .bash(HookEvent::PostToolUse, command, &fixture.repo)
        .await
        .expect("work");
    assert_sticky_failure(&h, &report, "project registry unreadable");
}
