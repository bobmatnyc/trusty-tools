use super::*;
use std::future::IntoFuture;

/// Spawn the daemon's real HTTP API on a random loopback port.
///
/// Why: lets the executor be tested against the genuine daemon routes
/// without a live daemon, tmux, or external network.
/// What: builds `api::router(DaemonState::with_root_isolated_managed(...))`,
/// binds an ephemeral port, serves it on a background task, and returns the
/// state plus base URL. The managed-session store is rooted in a throwaway
/// temp directory and backed by a no-op tmux driver so `reconcile_on_boot`
/// never adopts the operator's live `tmpm-*` sessions into the test store
/// (#1734). The framework root (pairing, audit log) is also pointed at the
/// temp dir so tests never read or write the operator's real `~/.trusty-mpm`.
/// Test: used by the `execute_*` tests below.
async fn spawn_test_daemon() -> (std::sync::Arc<crate::daemon::state::DaemonState>, String) {
    use crate::daemon::{api, state::DaemonState};
    // `keep` converts TempDir into a PathBuf that persists beyond this scope
    // so the directory outlives the background server task.
    let root = tempfile::tempdir().unwrap().keep();
    let state = std::sync::Arc::new(DaemonState::with_root_isolated_managed(root).await);
    let router = api::router(std::sync::Arc::clone(&state));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(axum::serve(listener, router).into_future());
    (state, format!("http://{addr}"))
}

/// Point `$HOME` at a temp directory and restore it on drop, panic or not.
///
/// Why: the daemon's workspace-removal path resolves the containment root from
/// `$HOME`, so a decommission test that wants a removal to actually happen must
/// seed its workspace under a redirected root. The redirect is process-wide, so
/// every user is `#[serial_test::serial]` and every user needs the restore to
/// survive a failed assertion.
/// What: `redirect` swaps `$HOME` and returns the guard holding the prior value;
/// `Drop` puts it back (or removes the variable when there was none).
/// Test: used by `executor_decommission_reports_daemon_workspace_verdict` and
/// `decommission_managed_id_prunes_stale_worktree_bookkeeping`.
struct HomeGuard(Option<String>);

impl HomeGuard {
    fn redirect(to: &std::path::Path) -> Self {
        let prior = std::env::var("HOME").ok();
        // SAFETY: every caller is serialized via `#[serial_test::serial]`.
        unsafe { std::env::set_var("HOME", to) };
        Self(prior)
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: serialized via `#[serial_test::serial]`.
        match self.0 {
            Some(ref p) => unsafe { std::env::set_var("HOME", p) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

#[tokio::test]
async fn execute_help_returns_help() {
    // The `/help` path is pure — no HTTP, no daemon.
    let executor = CommandExecutor::new("http://unused");
    match executor.execute(TrustyCommand::Help).await {
        CommandResult::Help(text) => assert!(text.contains("/sessions")),
        other => panic!("expected Help, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_sessions_against_test_daemon() {
    // With one registered session, `/sessions` returns exactly that summary.
    use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
    let (state, url) = spawn_test_daemon().await;
    let mut session = Session::new(SessionId::new(), "/tmp/proj", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    state.register_session(session);

    let executor = CommandExecutor::new(url);
    match executor.execute(TrustyCommand::Sessions).await {
        CommandResult::Sessions(list) => {
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].workdir, "/tmp/proj");
        }
        other => panic!("expected Sessions, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_kill_returns_killed() {
    // Registering a session then killing it yields `Killed`.
    use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
    let (state, url) = spawn_test_daemon().await;
    let id = SessionId::new();
    let mut session = Session::new(id, "/tmp/proj", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    state.register_session(session);

    let executor = CommandExecutor::new(url);
    match executor
        .execute(TrustyCommand::Kill {
            session_id: id.0.to_string(),
        })
        .await
    {
        CommandResult::Killed { session_id } => assert_eq!(session_id, id.0.to_string()),
        other => panic!("expected Killed, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_kill_unknown_session_errors() {
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor
        .execute(TrustyCommand::Kill {
            session_id: uuid::Uuid::new_v4().to_string(),
        })
        .await
    {
        CommandResult::Error(msg) => assert!(msg.contains("not found")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_approve_unknown_session_errors() {
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor
        .execute(TrustyCommand::Approve {
            session_id: uuid::Uuid::new_v4().to_string(),
        })
        .await
    {
        CommandResult::Error(msg) => assert!(msg.contains("not found")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_approve_known_session() {
    use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
    let (state, url) = spawn_test_daemon().await;
    let id = SessionId::new();
    let mut session = Session::new(id, "/tmp/proj", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    state.register_session(session);

    let executor = CommandExecutor::new(url);
    match executor
        .execute(TrustyCommand::Approve {
            session_id: id.0.to_string(),
        })
        .await
    {
        CommandResult::Approved { session_id } => assert_eq!(session_id, id.0.to_string()),
        other => panic!("expected Approved, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_projects_against_test_daemon() {
    // `/projects` returns a well-formed (possibly empty) discovered list.
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor.execute(TrustyCommand::Projects).await {
        CommandResult::DiscoveredProjects(list) => {
            for p in &list {
                assert!(!p.path.is_empty());
            }
        }
        other => panic!("expected DiscoveredProjects, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_discover_against_test_daemon() {
    // `/discover` returns a well-formed count (zero when tmux is absent on
    // CI), never an error against a live daemon.
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor.execute(TrustyCommand::Discover).await {
        CommandResult::Discovered { count } => {
            // Count is a usize; the call simply must succeed.
            let _ = count;
        }
        other => panic!("expected Discovered, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_adopt_unknown_session_errors() {
    // Adopting a session that does not exist (or with tmux unavailable on
    // CI) reports an error rather than a success.
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor
        .execute(TrustyCommand::Adopt {
            session: "no-such-session-xyz".into(),
        })
        .await
    {
        CommandResult::Error(_) => {}
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn register_project_succeeds() {
    // The `[Set Active]` flow registers a project by path.
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor.register_project("/work/discovered-demo").await {
        CommandResult::ProjectRegistered { path } => {
            assert_eq!(path, "/work/discovered-demo");
        }
        other => panic!("expected ProjectRegistered, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_doctor_against_test_daemon() {
    // `/doctor` round-trips a real report through the client executor.
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor.execute(TrustyCommand::Doctor).await {
        CommandResult::Doctor(report) => {
            // This test owns the EXECUTOR round-trip, not the check roster.
            // The exact count is pinned by `run_doctor_produces_thirty_eight_checks`
            // and `doctor_endpoint_returns_report`, both of which derive it from
            // their own name list — duplicating a bare literal here just made it a
            // fourth place to forget (#4090 review LOW-1).
            assert!(
                !report.checks.is_empty(),
                "a live daemon must return a non-empty doctor report"
            );
            let names: Vec<&str> = report.checks.iter().map(|c| c.name.as_str()).collect();
            for expected in ["instructions", "worktrees", "push_guard"] {
                assert!(
                    names.contains(&expected),
                    "the round-tripped report must carry the `{expected}` check, got: {names:?}"
                );
            }
        }
        other => panic!("expected Doctor, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_overseer_returns_status() {
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor.execute(TrustyCommand::Overseer).await {
        CommandResult::OverseerStatus { handler, .. } => assert!(!handler.is_empty()),
        other => panic!("expected OverseerStatus, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_status_no_events() {
    use crate::core::session::{ControlModel, Session, SessionId, SessionStatus};
    let (state, url) = spawn_test_daemon().await;
    let id = SessionId::new();
    let mut session = Session::new(id, "/tmp/proj", ControlModel::Tmux, None);
    session.status = SessionStatus::Active;
    state.register_session(session);

    let executor = CommandExecutor::new(url);
    match executor
        .execute(TrustyCommand::Status {
            session_id: id.0.to_string(),
        })
        .await
    {
        CommandResult::SessionDetail { events, .. } => assert!(events.is_empty()),
        other => panic!("expected SessionDetail, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_health_against_test_daemon() {
    // Against a live daemon the `health` verb reports reachable=true, a status
    // word, and a fleet summary (empty fleet → zero counts) — never an error.
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url.clone());
    match executor.execute(TrustyCommand::Health).await {
        CommandResult::Health(report) => {
            assert!(report.reachable, "daemon should be reachable");
            assert!(!report.status.is_empty());
            assert_eq!(report.url, url);
            assert_eq!(report.managed_total, 0);
            assert_eq!(report.managed_pending_decisions, 0);
        }
        other => panic!("expected Health, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_health_excludes_deleted_tombstones_from_managed_total() {
    // #3034 fix-round HIGH-2: a slot tombstone (`deleted: true`) is intentionally
    // shown in `tm ls`, but it is not a session an operator can act on — `health`
    // must count only live sessions, never inflate `managed_total` with
    // tombstones the daemon reports for stable-slot numbering.
    use std::path::PathBuf;
    let (state, url) = spawn_test_daemon().await;
    let mgr = state.session_manager().await;

    let live = mgr
        .create(
            "keep me".into(),
            Some(PathBuf::from("/tmp/wt-live")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create live session");
    let gone = mgr
        .create(
            "delete me".into(),
            Some(PathBuf::from("/tmp/wt-gone")),
            None,
            None,
            None,
            None,
        )
        .await
        .expect("create session to delete");
    // Observe both into the slot registry, then delete AND compact `gone` so
    // the next list renders it as a `deleted: true` tombstone at its
    // reserved slot. `delete_record` alone (#2012/#3302) is a SOFT delete —
    // the record stays in the store, marked `Deleted` — so a genuine #3034
    // slot tombstone (`record: None`) additionally requires the permanent-
    // removal primitive `compact_record`, mirroring the real two-step CLI
    // path (`tm sessions delete` then `tm sessions prune --state deleted`,
    // which calls the same primitive internally).
    mgr.numbered_snapshot(&mgr.list().await).await;
    mgr.delete_record(&gone.id, true)
        .await
        .expect("delete gone session");
    mgr.compact_record(&gone.id)
        .await
        .expect("compact the soft-deleted session out of the store");

    let executor = CommandExecutor::new(url);
    match executor.execute(TrustyCommand::Health).await {
        CommandResult::Health(report) => {
            assert!(report.reachable, "daemon should be reachable");
            assert_eq!(
                report.managed_total, 1,
                "the deleted slot's tombstone must not be counted alongside the live session {}",
                live.id
            );
            assert_eq!(report.managed_pending_decisions, 0);
        }
        other => panic!("expected Health, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_health_dead_daemon_renders() {
    // A dead daemon must render as a Health result with reachable=false — never a
    // panic and never a transport Error.
    let executor = CommandExecutor::new("http://127.0.0.1:0");
    match executor.execute(TrustyCommand::Health).await {
        CommandResult::Health(report) => {
            assert!(!report.reachable, "dead daemon must be reachable=false");
            assert_eq!(report.status, "unreachable");
            assert_eq!(report.managed_total, 0);
        }
        other => panic!("expected Health, got {other:?}"),
    }
}

#[test]
fn resolve_session_exact_and_prefix() {
    use crate::client::http_client::SessionRow;
    use crate::core::session::{SessionId, SessionStatus};
    let rows = vec![
        SessionRow {
            id: SessionId(uuid::Uuid::nil()),
            workdir: "/tmp/a".into(),
            status: SessionStatus::Active,
            active_delegations: 0,
            tmux_name: "tmpm-blue-fox".into(),
            last_seen: Default::default(),
        },
        SessionRow {
            id: SessionId(uuid::Uuid::from_u128(1)),
            workdir: "/tmp/b".into(),
            status: SessionStatus::Active,
            active_delegations: 0,
            tmux_name: "frontend".into(),
            last_seen: Default::default(),
        },
    ];
    // Exact friendly-name match.
    assert_eq!(
        resolve_session(&rows, "frontend").as_deref(),
        Some("frontend")
    );
    // Prefix match.
    assert_eq!(
        resolve_session(&rows, "tmpm-blue").as_deref(),
        Some("tmpm-blue-fox")
    );
    // Exact id match resolves to the friendly name.
    assert_eq!(
        resolve_session(&rows, &uuid::Uuid::nil().to_string()).as_deref(),
        Some("tmpm-blue-fox")
    );
    assert!(resolve_session(&rows, "no-such").is_none());
}

#[test]
fn truncate_output_caps_long_text() {
    let short = "hello";
    assert_eq!(truncate_output(short), short);
    let long = "x".repeat(MAX_OUTPUT_CHARS + 100);
    let truncated = truncate_output(&long);
    assert!(truncated.contains("output truncated"));
    assert!(truncated.chars().count() <= MAX_OUTPUT_CHARS + 32);
}

#[test]
fn decide_answer_prefers_proposed_default() {
    // Approve adopts the harness's proposed default when present, else "yes";
    // deny is always "no" regardless of any proposed default.
    assert_eq!(decide_answer(true, Some("option B")), "option B");
    assert_eq!(decide_answer(true, None), "yes");
    assert_eq!(decide_answer(false, Some("option B")), "no");
    assert_eq!(decide_answer(false, None), "no");
}

#[tokio::test]
async fn execute_approve_managed_miss_then_project_not_found() {
    // The 1A round-trip follow-up: with no managed session matching, `decide`
    // falls through to the project `GET /sessions` path exactly once and reports
    // not-found for an unknown id (it does NOT masquerade as a managed error).
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor
        .execute(TrustyCommand::Approve {
            session_id: uuid::Uuid::new_v4().to_string(),
        })
        .await
    {
        CommandResult::Error(msg) => assert!(msg.contains("not found")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_send_unknown_session_errors() {
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor
        .execute(TrustyCommand::Send {
            session: "no-such-session".into(),
            prompt: "hello".into(),
        })
        .await
    {
        CommandResult::Error(msg) => assert!(msg.contains("not found")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_connect_errors_when_daemon_unreachable() {
    // `/connect` registers via `POST /api/v1/sessions/connect`; with no
    // daemon the failure surfaces as a renderable `Error`, never a panic.
    let executor = CommandExecutor::new("http://127.0.0.1:0");
    match executor
        .execute(TrustyCommand::Connect {
            project: "/tmp/no-such-project".into(),
            session_name: None,
        })
        .await
    {
        CommandResult::Error(msg) => assert!(msg.contains("connect failed")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_launch_errors_when_daemon_unreachable() {
    // `/launch` registers via `POST /sessions`; with no daemon the failure
    // surfaces as a renderable `Error`.
    let executor = CommandExecutor::new("http://127.0.0.1:0");
    match executor
        .execute(TrustyCommand::Launch {
            project: "/tmp/no-such-project".into(),
            session_name: None,
        })
        .await
    {
        CommandResult::Error(msg) => assert!(msg.contains("launch failed")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_send_empty_prompt_errors() {
    let executor = CommandExecutor::new("http://unused");
    match executor
        .execute(TrustyCommand::Send {
            session: "frontend".into(),
            prompt: "   ".into(),
        })
        .await
    {
        CommandResult::Error(msg) => assert!(msg.contains("prompt")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_managed_adopt_requires_tmux_name() {
    // Validation is pure — no daemon needed. An empty tmux_name is rejected with a
    // renderable error before any HTTP call (#1433).
    let executor = CommandExecutor::new("http://unused");
    match executor
        .execute(TrustyCommand::ManagedAdopt {
            tmux_name: "  ".into(),
            cwd: "/x".into(),
            task: None,
            runtime: None,
        })
        .await
    {
        CommandResult::Error(msg) => assert!(msg.contains("tmux_name")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_managed_adopt_requires_cwd() {
    // The cwd is required because the pane's provenance is unknown to the daemon;
    // an empty cwd is rejected before any HTTP call (#1433).
    let executor = CommandExecutor::new("http://unused");
    match executor
        .execute(TrustyCommand::ManagedAdopt {
            tmux_name: "tmpm-x".into(),
            cwd: "".into(),
            task: None,
            runtime: None,
        })
        .await
    {
        CommandResult::Error(msg) => assert!(msg.contains("cwd")),
        other => panic!("expected Error, got {other:?}"),
    }
}

/// #5899: `ManagedDecommission` must carry the daemon's workspace verdict into
/// [`CommandResult::ManagedLifecycle`] — for a workspace it deleted AND for one it
/// left alone.
///
/// The verdict was computed and sent correctly all along; the executor lost it,
/// because the response decoded into a summary type with no field for it. This runs
/// the real daemon route so a key rename daemon-side fails here rather than
/// deserializing quietly to `None`.
///
/// `$HOME` is redirected for the duration: the daemon's removal path applies a
/// containment guard against the configured workspace root, so a tm-owned workspace
/// anywhere else would be refused and the removal arm would prove nothing. Serial,
/// because that redirect is process-wide.
#[tokio::test]
#[serial_test::serial]
async fn executor_decommission_reports_daemon_workspace_verdict() {
    use crate::runtime::RuntimeKind;
    use crate::session_manager::ManagedSessionId;

    let home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeGuard::redirect(home.path());
    let workspace_root = trusty_common::workspace_layout::resolve_workspace_root(None);

    for owned in [true, false] {
        let (state, url) = spawn_test_daemon().await;
        let id = ManagedSessionId::new();
        let ws = workspace_root.join(format!("tm-5899-{id}"));
        std::fs::create_dir_all(&ws).expect("create seeded workspace");
        state
            .session_manager()
            .await
            .create_with_id(
                id,
                "regression: #5899 executor verdict".to_string(),
                Some(ws.clone()),
                None,
                Some(ws.clone()),
                None,
                None,
                RuntimeKind::default(),
                false,
                owned,
            )
            .await
            .expect("seed session");

        let result = CommandExecutor::new(url)
            .execute(TrustyCommand::ManagedDecommission {
                target: id.to_string(),
            })
            .await;
        match result {
            CommandResult::ManagedLifecycle {
                action,
                workspace_removed,
                ..
            } => {
                assert_eq!(action, "decommissioned");
                assert_eq!(
                    workspace_removed,
                    Some(owned),
                    "owned={owned}: the executor must forward the daemon's verdict"
                );
                assert_eq!(
                    ws.exists(),
                    !owned,
                    "owned={owned}: the verdict must match the filesystem"
                );
            }
            other => panic!("expected ManagedLifecycle, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&ws);
    }
}

/// #5913: pin which decommission outcomes run `git worktree prune`, so a later
/// change cannot flip the decision silently.
///
/// The daemon has two removal branches and only one leaves git's bookkeeping
/// stale. It removes an in-project worktree it does NOT own with `git worktree
/// remove` — that prunes daemon-side, and the response carries no
/// `workspace_path_was`. It removes a workspace tm OWNED with a plain
/// `remove_dir_all`, which git never learns about, and reports the path. So the
/// path's presence, not a caller-supplied flag, is what selects the prune.
#[test]
fn worktree_prune_dir_fires_only_on_a_named_removal() {
    use crate::client::ManagedDecommissionOutcome;

    let outcome = |removed: serde_json::Value, was: serde_json::Value| {
        serde_json::from_value::<ManagedDecommissionOutcome>(serde_json::json!({
            "id": "11111111-2222-3333-4444-555555555555",
            "name": "tm-5913",
            "state": "decommissioned",
            "workspace_removed": removed,
            "workspace_path_was": was,
        }))
        .expect("fixture must match the daemon's wire shape")
    };
    let named = serde_json::json!("/base/.worktrees/tm-5913");
    let parent = Some(std::path::PathBuf::from("/base/.worktrees"));

    assert_eq!(
        super::managed::worktree_prune_dir(&outcome(serde_json::json!(true), named.clone())),
        parent,
        "a removal the daemon named left git's bookkeeping stale"
    );
    assert_eq!(
        super::managed::worktree_prune_dir(&outcome(
            serde_json::json!(true),
            serde_json::json!(null)
        )),
        None,
        "the daemon already pruned the branch it reports no path for"
    );
    assert_eq!(
        super::managed::worktree_prune_dir(&outcome(serde_json::json!(false), named.clone())),
        None,
        "nothing was deleted, so nothing is stale"
    );
    assert_eq!(
        super::managed::worktree_prune_dir(&outcome(serde_json::json!(null), named)),
        None,
        "an absent verdict is never read as a removal (#5899)"
    );
}

/// #5913: the routed `tm session decommission <id>` must leave the base repo's
/// worktree bookkeeping clean, exactly as the bulk prune sweep already did.
///
/// This is the live asymmetry the issue reports, reproduced whole. The daemon
/// removes a tm-owned workspace with `remove_dir_all`; git never learns about it,
/// so the entry survives in `git worktree list` until something prunes it. The
/// bulk path pruned and the routed path did not, because each reached the
/// endpoint by its own route. Against the pre-#5913 routed path this fails: the
/// workspace is gone and the stale entry is still listed.
///
/// `$HOME` is redirected because the daemon's removal path applies a containment
/// guard against the configured workspace root, so the base repo lives under that
/// root. Serial, because the redirect is process-wide.
#[tokio::test]
#[serial_test::serial]
async fn decommission_managed_id_prunes_stale_worktree_bookkeeping() {
    use crate::runtime::RuntimeKind;
    use crate::session_manager::ManagedSessionId;

    /// Run `git` in `dir`, failing the test on a nonzero exit.
    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed to start: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    let home = tempfile::TempDir::new().unwrap();
    let _home_guard = HomeGuard::redirect(home.path());
    let workspace_root = trusty_common::workspace_layout::resolve_workspace_root(None);

    let base = workspace_root.join("tm-5913-base");
    std::fs::create_dir_all(&base).expect("create base repo dir");
    git(&base, &["init", "--quiet"]);
    git(&base, &["config", "user.email", "tm@example.invalid"]);
    git(&base, &["config", "user.name", "tm test"]);
    std::fs::write(base.join("README.md"), "seed\n").unwrap();
    git(&base, &["add", "README.md"]);
    git(&base, &["commit", "--quiet", "-m", "seed"]);

    let id = ManagedSessionId::new();
    let leaf = format!("tm-5913-{id}");
    let ws = base.join(".worktrees").join(&leaf);
    git(
        &base,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            &format!("session/{leaf}"),
            &ws.to_string_lossy(),
        ],
    );
    assert!(
        git(&base, &["worktree", "list", "--porcelain"]).contains(&leaf),
        "fixture invariant: the worktree must be registered before decommission"
    );

    let (state, url) = spawn_test_daemon().await;
    state
        .session_manager()
        .await
        .create_with_id(
            id,
            "regression: #5913 decommission worktree bookkeeping".to_string(),
            Some(ws.clone()),
            None,
            Some(ws.clone()),
            None,
            None,
            RuntimeKind::default(),
            false,
            // Owned: the daemon removes it with `remove_dir_all`, which is the
            // branch that leaves git's bookkeeping behind.
            true,
        )
        .await
        .expect("seed session");

    let result = CommandExecutor::new(url)
        .execute(TrustyCommand::ManagedDecommission {
            target: id.to_string(),
        })
        .await;
    match result {
        CommandResult::ManagedLifecycle {
            workspace_removed, ..
        } => assert_eq!(
            workspace_removed,
            Some(true),
            "fixture invariant: the daemon must have removed the owned workspace"
        ),
        other => panic!("expected ManagedLifecycle, got {other:?}"),
    }
    assert!(!ws.exists(), "the workspace must be gone: {}", ws.display());
    assert!(
        !git(&base, &["worktree", "list", "--porcelain"]).contains(&leaf),
        "the routed decommission left a stale worktree entry for {leaf} — the \
         bookkeeping repair did not run"
    );

    let _ = std::fs::remove_dir_all(&base);
}

#[tokio::test]
async fn pair_request_returns_code() {
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor.pair_request().await {
        CommandResult::PairCode { code, .. } => assert_eq!(code.len(), 6),
        other => panic!("expected PairCode, got {other:?}"),
    }
}

#[tokio::test]
async fn pair_confirm_unknown_code_errors() {
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor.pair_confirm("ZZZZZZ", 999).await {
        CommandResult::Error(msg) => assert!(msg.contains("invalid")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_managed_list_against_test_daemon() {
    // With no managed sessions provisioned, `managed-list` returns an empty
    // (but well-formed) list against the live managed route — never an error.
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor.execute(TrustyCommand::ManagedList).await {
        CommandResult::ManagedSessions(list) => assert!(list.is_empty()),
        other => panic!("expected ManagedSessions, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_managed_get_unknown_errors() {
    // A target that resolves against no managed session is a renderable error,
    // not a panic.
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor
        .execute(TrustyCommand::ManagedGet {
            target: "no-such-managed".into(),
        })
        .await
    {
        CommandResult::Error(msg) => assert!(msg.contains("not found")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_managed_answer_unknown_errors() {
    // The corrected decision-answer path: answering an unknown managed session
    // resolves to a not-found error (it never falls through to a synthetic hook).
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor
        .execute(TrustyCommand::ManagedAnswer {
            target: "no-such-managed".into(),
            answer: "yes".into(),
        })
        .await
    {
        CommandResult::Error(msg) => assert!(msg.contains("not found")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_managed_answer_empty_errors() {
    // An empty answer is rejected before any HTTP call.
    let executor = CommandExecutor::new("http://unused");
    match executor
        .execute(TrustyCommand::ManagedAnswer {
            target: "x".into(),
            answer: "   ".into(),
        })
        .await
    {
        CommandResult::Error(msg) => assert!(msg.contains("answer")),
        other => panic!("expected Error, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_managed_lifecycle_unknown_errors() {
    // stop / resume / decommission of an unknown managed target all error.
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    for cmd in [
        TrustyCommand::ManagedRuntimeStop {
            target: "no-such".into(),
        },
        TrustyCommand::ManagedResume {
            target: "no-such".into(),
        },
        TrustyCommand::ManagedDecommission {
            target: "no-such".into(),
        },
    ] {
        match executor.execute(cmd).await {
            CommandResult::Error(msg) => assert!(msg.contains("not found")),
            other => panic!("expected Error, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn execute_managed_verbs_error_when_daemon_unreachable() {
    // Every target-taking managed verb surfaces an unreachable daemon as a
    // renderable Error rather than panicking.
    let executor = CommandExecutor::new("http://127.0.0.1:0");
    let cmds = [
        TrustyCommand::ManagedGet { target: "x".into() },
        TrustyCommand::ManagedActivity { target: "x".into() },
        TrustyCommand::ManagedAttachCmd { target: "x".into() },
        TrustyCommand::ManagedSend {
            target: "x".into(),
            text: "hi".into(),
        },
    ];
    for cmd in cmds {
        match executor.execute(cmd).await {
            CommandResult::Error(_) => {}
            other => panic!("expected Error, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn pair_request_then_confirm_succeeds() {
    // The full handshake: request a code, confirm it, then status is paired.
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    let code = match executor.pair_request().await {
        CommandResult::PairCode { code, .. } => code,
        other => panic!("expected PairCode, got {other:?}"),
    };
    match executor.pair_confirm(&code, 424242).await {
        CommandResult::PairSuccess { chat_info } => assert!(chat_info.contains("424242")),
        other => panic!("expected PairSuccess, got {other:?}"),
    }
    match executor.execute(TrustyCommand::Start).await {
        CommandResult::PairState { paired } => assert!(paired),
        other => panic!("expected PairState, got {other:?}"),
    }
}

#[tokio::test]
async fn execute_approve_errors_when_managed_list_unreachable() {
    // Regression for the swallowed-daemon-error fix: `decide()` fetches the
    // managed list first. When that fetch fails (daemon down), the operator
    // must see a transport error, NOT a misleading "session not found" from a
    // silent fall-through to the project-session path.
    let executor = CommandExecutor::new("http://127.0.0.1:0");
    match executor
        .execute(TrustyCommand::Approve {
            session_id: uuid::Uuid::new_v4().to_string(),
        })
        .await
    {
        CommandResult::Error(msg) => {
            assert!(
                msg.contains("daemon unreachable"),
                "expected a daemon-unreachable error, got: {msg}"
            );
            assert!(
                !msg.contains("not found"),
                "must not masquerade a transport failure as not-found: {msg}"
            );
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn is_session_id_detects_uuid_vs_name() {
    // A canonical id (UUID) is treated as an id so `managed_get` can skip the
    // list round-trip; a friendly name or prefix is not.
    assert!(super::managed::is_session_id(
        &uuid::Uuid::new_v4().to_string()
    ));
    assert!(super::managed::is_session_id(
        "367c6c51-1025-419c-b6d6-be9a753e8914"
    ));
    assert!(!super::managed::is_session_id("brave-otter"));
    assert!(!super::managed::is_session_id("brave"));
    assert!(!super::managed::is_session_id(""));
}
