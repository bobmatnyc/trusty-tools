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
    // `/doctor` returns a seven-check report against a live daemon.
    let (_state, url) = spawn_test_daemon().await;
    let executor = CommandExecutor::new(url);
    match executor.execute(TrustyCommand::Doctor).await {
        CommandResult::Doctor(report) => {
            // #1840 added the worktrees check (6); DOC-28 R4(a) added the
            // output_style check, bringing the total to 7.
            assert_eq!(report.checks.len(), 7);
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
