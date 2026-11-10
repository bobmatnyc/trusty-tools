//! Socket-level tests for the hook-event producer (#6849).
//!
//! Why: DOC-73 §4.1's non-blocking invariant and §3.2's by-reference rule are
//! claims about bytes on a socket and time on the hook path, so they are
//! proven against a real listener: an absent bus, a stalled one, an
//! unresolvable one, and the raw frames a working one receives.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use trusty_common::control_bus::{
    ActionEvent, CallPhase, DEFAULT_PUSH_BUFFER_CAPACITY, HarnessEvent, HarnessPayload,
    PushFlushOutcome,
};

use super::*;
use crate::core::hook::{HookEvent, HookEventRecord};
use crate::core::session::SessionId;

const RECORDED: &str = include_str!("../../../testdata/hook_session_6849.ndjson");

/// A flush interval no test outlives: the task exists but never drains.
const NO_TICK: Duration = Duration::from_secs(3600);

fn sock_in(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("bus.sock")
}

/// A daemon state with a publisher aimed at `sock` and no flush task.
fn attached(sock: &Path) -> (DaemonState, Arc<ActionPublisher>) {
    let state = DaemonState::new();
    let publisher = Arc::new(ActionPublisher::new(sock).with_home(Some("/Users/dev".into())));
    assert!(state.action_events().attach(Arc::clone(&publisher)));
    (state, publisher)
}

fn pre_tool_use(session: SessionId, id: &str) -> HookEventRecord {
    HookEventRecord::now(
        session,
        HookEvent::PreToolUse,
        json!({"cwd": "/w", "tool": "Bash", "tool_use_id": id, "input": {"command": "ls"}}),
    )
}

/// Accept one connection on `sock` and return every byte it carried.
fn read_one(sock: &Path) -> tokio::task::JoinHandle<Vec<u8>> {
    let listener = trusty_common::uds::bind_hardened(sock).expect("bind");
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        conn.read_to_end(&mut buf).await.expect("read");
        buf
    })
}

/// What `read_one` collected, bounded so a red run fails rather than hangs.
async fn received(read: tokio::task::JoinHandle<Vec<u8>>) -> Vec<u8> {
    tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .expect("nothing reached the bus within 10 s")
        .expect("join")
}

fn frames(bytes: &[u8]) -> Vec<HarnessEvent> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|l| serde_json::from_str(l).expect("frame"))
        .collect()
}

/// A hook pushed through the daemon's one ingest point reaches the bus as
/// typed actions joined by `parent_id`.
#[tokio::test]
async fn hook_events_reach_the_bus_with_a_parent_chain_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let (state, publisher) = attached(&sock);
    let s = SessionId::new();
    state.push_hook_event(pre_tool_use(s, "toolu_9"));
    state.push_hook_event(HookEventRecord::now(
        s,
        HookEvent::PostToolUse,
        json!({"tool": "Bash", "tool_use_id": "toolu_9"}),
    ));
    let read = read_one(&sock);
    let outcome = publisher.flush().await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert!(outcome.sent > 0, "nothing was buffered to send");
    let got = frames(&received(read).await);
    assert_eq!(got.len(), 3, "inferred root, started, finished: {got:?}");
    assert_eq!(got[0].parent_id, None);
    assert_eq!(got[1].parent_id, Some(got[0].id));
    assert_eq!(got[2].parent_id, Some(got[1].id));
    let phases: Vec<_> = got[1..]
        .iter()
        .map(|e| match &e.payload {
            HarnessPayload::Action(ActionEvent::Tool { phase, call_id, .. }) => {
                assert_eq!(call_id, "toolu_9");
                *phase
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(phases, [CallPhase::Started, CallPhase::Finished]);
    assert!(got.windows(2).all(|w| w[0].seq < w[1].seq), "seq rises");
    // The ring buffer still records the hook as before.
    assert_eq!(state.hook_events_for(s).len(), 2);
}

/// Fail-soft, absent bus: 10,000 hooks stay cheap, the buffer holds its
/// bound, and the overflow is counted rather than queued.
#[test]
fn ten_thousand_hooks_with_no_bus_are_fast_and_bounded_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (state, publisher) = attached(&sock_in(&dir));
    let s = SessionId::new();
    let started = Instant::now();
    for i in 0..10_000 {
        state.push_hook_event(pre_tool_use(s, &format!("t{i}")));
    }
    // 5 s, not 1 s: opt-level 0 on shared CI runners (as #9078).
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(publisher.buffered_len(), DEFAULT_PUSH_BUFFER_CAPACITY);
    // 10,000 tool events plus the inferred session root.
    assert_eq!(
        publisher.dropped(),
        (10_001 - DEFAULT_PUSH_BUFFER_CAPACITY) as u64
    );
}

/// Fail-soft, stalled bus: a flush stuck on a peer that never reads does not
/// slow the hook path, and the flush itself gives up within its timeout.
#[tokio::test]
async fn a_stalled_bus_never_slows_hook_ingest_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let (state, publisher) = attached(&sock);
    let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
    let hold = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(conn);
    });
    let s = SessionId::new();
    for i in 0..3_000 {
        state.push_hook_event(pre_tool_use(s, &format!("fill{i}")));
    }
    let p = Arc::clone(&publisher);
    let flush = tokio::spawn(async move { p.flush().await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let started = Instant::now();
    for i in 0..1_000 {
        state.push_hook_event(pre_tool_use(s, &format!("live{i}")));
    }
    let ingest = started.elapsed();
    let outcome: PushFlushOutcome = flush.await.expect("join");
    hold.abort();
    assert!(
        outcome.error.is_some(),
        "the peer never read, so the flush stalls"
    );
    assert!(
        ingest < Duration::from_secs(2),
        "hook ingest waited on the bus: {ingest:?}"
    );
    assert!(publisher.buffered_len() > 0, "unsent events stay buffered");
}

/// Fail-soft, unresolvable bus: no publisher, a warning, and hooks are
/// recorded exactly as before.
#[tokio::test]
async fn an_unresolvable_bus_still_ingests_hooks_6849() {
    // The control: the same call with a resolvable socket does attach, so
    // the `false` below is the error arm, not a publisher that never attaches.
    let dir = tempfile::tempdir().expect("tempdir");
    let control = DaemonState::new();
    assert!(
        start_with(&control, true, NO_TICK, || Ok(sock_in(&dir))),
        "a resolvable bus attaches"
    );
    control.action_events().shutdown().await;

    let state = DaemonState::new();
    assert!(!start_with(&state, true, NO_TICK, || anyhow::bail!(
        "no data directory"
    )));
    assert!(state.action_events().publisher().is_none());
    let s = SessionId::new();
    state.push_hook_event(pre_tool_use(s, "t"));
    assert_eq!(state.hook_events_for(s).len(), 1);
    state.action_events().shutdown().await;
}

/// A daemon isolated from the host (a scratch root or `$HOME`) whose data dir
/// is the host's must not dial the host's bus; the resolver is never called.
#[tokio::test]
async fn a_scratch_daemon_dials_no_host_bus_6849() {
    assert!(!bus_allowed(false, false));
    assert!(
        bus_allowed(true, false),
        "an overridden data dir is its own bus"
    );
    assert!(bus_allowed(false, true), "the operator's daemon");
    assert!(bus_allowed(true, true));
    let state = DaemonState::new();
    let called = AtomicBool::new(false);
    let attached = start_with(&state, false, NO_TICK, || {
        called.store(true, Ordering::SeqCst);
        Ok(PathBuf::from("/nonexistent/bus.sock"))
    });
    assert!(!attached);
    assert!(!called.load(Ordering::SeqCst));
}

/// Every exit path delivers what is buffered: `shutdown` ends the flush task
/// and runs the final flush, and a second call (the binary's signal handler
/// and the serve drain both call it) is a prompt no-op.
#[tokio::test]
async fn shutdown_runs_the_final_flush_once_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let state = DaemonState::new();
    // The flush task never ticks here, so only the final flush can deliver.
    assert!(start_with(&state, true, NO_TICK, || Ok(sock.clone())));
    let s = SessionId::new();
    state.push_hook_event(pre_tool_use(s, "t1"));
    let read = read_one(&sock);
    state.action_events().shutdown().await;
    let got = frames(&received(read).await);
    assert_eq!(got.len(), 2, "root and started: {got:?}");
    let again = Instant::now();
    state.action_events().shutdown().await;
    assert!(again.elapsed() < Duration::from_millis(500));
}

/// The final flush is bounded even when the bus accepts and never reads.
#[tokio::test]
async fn final_flush_is_bounded_when_the_bus_stalls_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let (state, publisher) = attached(&sock);
    let s = SessionId::new();
    for i in 0..3_000 {
        state.push_hook_event(pre_tool_use(s, &format!("t{i}")));
    }
    assert!(publisher.buffered_len() > 1_000);
    let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
    let hold = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(conn);
    });
    let started = Instant::now();
    publisher.final_flush().await;
    hold.abort();
    assert!(
        started.elapsed() < FINAL_FLUSH_BUDGET + Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

/// Privacy, raw bytes: the recorded session's frames, as written to the
/// socket, hold no prompt, tool input or response, transcript path, message
/// or absolute home path.
#[tokio::test]
async fn projected_frames_carry_no_payload_bytes_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let (state, publisher) = attached(&sock);
    for line in RECORDED.lines() {
        let record: HookEventRecord = serde_json::from_str(line).expect("fixture");
        state.push_hook_event(record);
    }
    let read = read_one(&sock);
    let outcome = publisher.flush().await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert!(outcome.sent > 0, "nothing was buffered to send");
    let bytes = received(read).await;
    assert_eq!(frames(&bytes).len(), 21);
    for needle in [
        "SENTINEL",
        "/Users/dev",
        "transcript",
        "\"input\"",
        "\"prompt\"",
        "tool_response",
    ] {
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
            "{needle} reached the bus"
        );
    }
    // Every frame is a line of JSON carrying a domain the bus knows.
    for line in String::from_utf8_lossy(&bytes).lines() {
        let v: Value = serde_json::from_str(line).expect("json");
        assert!(matches!(
            v["payload"]["domain"].as_str(),
            Some("action" | "hook")
        ));
    }
}

/// One warning per outage, one notice on recovery, drops rate-limited.
#[test]
fn flush_health_warns_once_per_outage_6849() {
    let mut health = FlushHealth::default();
    let now = Instant::now();
    let down = || PushFlushOutcome {
        sent: 0,
        error: Some(trusty_common::uds::UdsRpcError::Timeout {
            path: PathBuf::from("/x"),
            timeout: Duration::from_millis(1),
        }),
    };
    let up = PushFlushOutcome {
        sent: 3,
        error: None,
    };
    assert_eq!(health.report(&down(), 0, now), [Notice::Down]);
    assert!(health.report(&down(), 0, now).is_empty());
    assert_eq!(health.report(&up, 0, now), [Notice::Recovered]);
    assert_eq!(health.report(&up, 5, now), [Notice::Dropped(5)]);
    assert!(
        health
            .report(&up, 9, now + Duration::from_secs(1))
            .is_empty()
    );
    assert_eq!(
        health.report(&up, 9, now + DROP_WARN_EVERY),
        [Notice::Dropped(9)]
    );
}

/// #6849 fix round: a hung bus holds tm's SIGTERM path for at most
/// [`SIGNAL_FLUSH_BUDGET`], so `process::exit` still beats the serve drain
/// (#6288). The peer accepts and never reads, so the flush
/// stalls mid-write; the budget is what bounds it.
#[tokio::test]
async fn the_signal_path_flush_returns_within_its_budget_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let state = DaemonState::new();
    assert!(start_with(&state, true, NO_TICK, || Ok(sock.clone())));
    let s = SessionId::new();
    for i in 0..3_000 {
        state.push_hook_event(pre_tool_use(s, &format!("t{i}")));
    }
    let listener = trusty_common::uds::bind_hardened(&sock).expect("bind");
    let hold = tokio::spawn(async move {
        let conn = listener.accept().await.expect("accept");
        tokio::time::sleep(Duration::from_secs(30)).await;
        drop(conn);
    });
    let started = Instant::now();
    before_signal_exit(&state, || {}).await;
    let took = started.elapsed();
    hold.abort();
    assert!(
        took < SIGNAL_FLUSH_BUDGET + Duration::from_millis(150),
        "the signal path waited {took:?} on a hung bus"
    );
}

/// #6849 fix round: the lock file goes only after the flush, so a spawner
/// never sees "no daemon" while this one still serves its socket.
#[tokio::test]
async fn the_signal_path_flushes_before_removing_the_lock_6849() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = sock_in(&dir);
    let state = DaemonState::new();
    assert!(start_with(&state, true, NO_TICK, || Ok(sock.clone())));
    state.push_hook_event(pre_tool_use(SessionId::new(), "t1"));
    let read = read_one(&sock);
    let publisher = Arc::clone(state.action_events().publisher().expect("attached"));
    let mut buffered_at_removal = None;
    before_signal_exit(&state, || {
        buffered_at_removal = Some(publisher.buffered_len());
    })
    .await;
    assert_eq!(
        buffered_at_removal,
        Some(0),
        "the lock went before the flush"
    );
    assert_eq!(frames(&received(read).await).len(), 2);
}

/// A git checkout at `base/name`, with `origin` when given, canonicalized.
fn checkout_10906(base: &Path, name: &str, origin: Option<&str>) -> PathBuf {
    let dir = base.join(name);
    std::fs::create_dir_all(dir.join("sub")).expect("mkdir");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q"]);
    if let Some(origin) = origin {
        git(&["remote", "add", "origin", origin]);
    }
    dir.canonicalize().expect("canonical")
}

/// Register `root` in the registry at `reg` through the store, which mints
/// the key, and return that key.
async fn register_10906(reg: &Path, name: &str, root: &Path) -> String {
    let mut store = crate::project::store::ProjectStore::load(reg)
        .await
        .expect("store");
    store
        .upsert(crate::project::Project {
            name: name.into(),
            repo_url: root.display().to_string(),
            default_branch: "main".into(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
            worktree: None,
            project_key: None,
        })
        .await
        .expect("upsert");
    store
        .get(name)
        .await
        .expect("get")
        .project_key
        .expect("key")
}

/// One `SessionStart` with `cwd`, through the daemon's ingest point; it
/// queues the session's project lookup (#10906).
fn start_in(state: &DaemonState, session: SessionId, cwd: &Path) {
    state.push_hook_event(HookEventRecord::now(
        session,
        HookEvent::SessionStart,
        json!({"cwd": cwd.display().to_string()}),
    ));
}

fn pre_tool_use_in(session: SessionId, cwd: &Path) -> HookEventRecord {
    HookEventRecord::now(
        session,
        HookEvent::PreToolUse,
        json!({"cwd": cwd.display().to_string(), "tool": "Bash", "tool_use_id": "t"}),
    )
}

/// #10906 regression: events from a registered project carry its registry
/// `project_key` on the wire — `owner/repo` with an origin, `local:<uuid>`
/// without — which is the field trusty-events keys `by_project` on (its
/// `events_console_metrics_answers_on_the_real_socket`). A session in an
/// unregistered directory goes out as `None`, so it counts as unattributed.
/// The `SessionStart` that queues the lookup goes out before it lands, as
/// `None`; the tool call after it carries the key.
#[tokio::test]
async fn an_event_from_a_registered_project_carries_its_project_key_10906() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = dir.path().join("registry");
    let web = checkout_10906(dir.path(), "web", Some("https://github.com/Acme/Web.git"));
    let solo = checkout_10906(dir.path(), "solo", None);
    let stray = checkout_10906(dir.path(), "stray", None);
    assert_eq!(register_10906(&reg, "web", &web).await, "acme/web");
    let solo_key = register_10906(&reg, "solo", &solo).await;
    assert!(solo_key.starts_with("local:"), "{solo_key}");
    let sock = sock_in(&dir);
    let state = DaemonState::new();
    let publisher =
        Arc::new(ActionPublisher::new(&sock).with_project_resolver(registry_resolver(reg.clone())));
    assert!(state.action_events().attach(Arc::clone(&publisher)));
    let (a, b, c) = (SessionId::new(), SessionId::new(), SessionId::new());
    // A subdirectory of the checkout resolves to the checkout's project.
    let cwds = [(a, web.join("sub")), (b, solo.clone()), (c, stray.clone())];
    for (s, cwd) in &cwds {
        start_in(&state, *s, cwd);
    }
    publisher.lookups_settled().await;
    for (s, cwd) in &cwds {
        state.push_hook_event(pre_tool_use_in(*s, cwd));
    }
    let read = read_one(&sock);
    let outcome = publisher.flush().await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    let bytes = received(read).await;
    let got = frames(&bytes);
    assert_eq!(got.len(), 6, "started and tool per session: {got:?}");
    let want = [
        (a, Some("acme/web")),
        (b, Some(solo_key.as_str())),
        (c, None),
    ];
    for (session, project) in want {
        let id = session.0.to_string();
        let mine: Vec<_> = got
            .iter()
            .filter(|e| e.session.as_deref() == Some(&id))
            .collect();
        assert_eq!(mine.len(), 2, "{id}: {got:?}");
        assert_eq!(mine[0].project, None, "queued, not yet resolved: {mine:?}");
        assert_eq!(mine[1].project.as_deref(), project, "{mine:?}");
    }
    let raw = String::from_utf8_lossy(&bytes);
    assert_eq!(raw.matches(r#""project":"acme/web""#).count(), 1, "{raw}");
}

/// #10906 error arm: a registry that cannot be read names no project, and
/// the session's events still go out at once, with `project: None`.
#[tokio::test]
async fn an_unresolvable_session_still_sends_its_events_unattributed_10906() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = dir.path().join("registry");
    std::fs::create_dir_all(&reg).expect("mkdir");
    std::fs::write(reg.join("projects.json"), "{not json").expect("write");
    let root = checkout_10906(dir.path(), "web", Some("https://github.com/acme/web"));
    let sock = sock_in(&dir);
    let state = DaemonState::new();
    let publisher =
        Arc::new(ActionPublisher::new(&sock).with_project_resolver(registry_resolver(reg)));
    assert!(state.action_events().attach(Arc::clone(&publisher)));
    let s = SessionId::new();
    start_in(&state, s, &root);
    publisher.lookups_settled().await;
    state.push_hook_event(pre_tool_use_in(s, &root));
    assert_eq!(publisher.buffered_len(), 2, "buffered on the hook path");
    let read = read_one(&sock);
    let outcome = publisher.flush().await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    let bytes = received(read).await;
    let got = frames(&bytes);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got.iter().all(|e| e.project.is_none()), "{got:?}");
    assert!(!String::from_utf8_lossy(&bytes).contains("\"project\""));
}

/// Commit in `checkout` and add a linked worktree of it at `at`,
/// returning the worktree's canonical path.
fn linked_worktree_10906(checkout: &Path, at: &Path, branch: &str) -> PathBuf {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c"])
            .arg("commit.gpgsign=false")
            .args(args)
            .output()
            .expect("git runs");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    // `worktree add` needs a commit to check out; a repeat is harmless.
    git(&["commit", "-q", "--allow-empty", "-m", "init"]);
    git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        branch,
        &at.display().to_string(),
    ]);
    std::fs::create_dir_all(at.join("sub")).expect("mkdir");
    at.canonicalize().expect("canonical")
}

/// The events every session in `cwds` sends, keyed by session id, through
/// [`registry_resolver`] on `reg`: a `SessionStart`, then, once the lookups
/// have landed, a tool call.
async fn projects_for_10906(
    dir: &tempfile::TempDir,
    reg: &Path,
    cwds: &[&Path],
) -> Vec<(String, Vec<Option<String>>)> {
    let sock = sock_in(dir);
    let state = DaemonState::new();
    let publisher = Arc::new(
        ActionPublisher::new(&sock).with_project_resolver(registry_resolver(reg.to_path_buf())),
    );
    assert!(state.action_events().attach(Arc::clone(&publisher)));
    let sessions: Vec<SessionId> = cwds.iter().map(|_| SessionId::new()).collect();
    for (s, cwd) in sessions.iter().zip(cwds) {
        start_in(&state, *s, cwd);
    }
    publisher.lookups_settled().await;
    for (s, cwd) in sessions.iter().zip(cwds) {
        state.push_hook_event(pre_tool_use_in(*s, cwd));
    }
    let read = read_one(&sock);
    let outcome = publisher.flush().await;
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    let got = frames(&received(read).await);
    assert_eq!(got.len(), 2 * cwds.len(), "{got:?}");
    sessions
        .iter()
        .map(|s| {
            let id = s.0.to_string();
            let projects = got
                .iter()
                .filter(|e| e.session.as_deref() == Some(&id))
                .map(|e| e.project.clone())
                .collect();
            (id, projects)
        })
        .collect()
}

/// #10906 regression: a session working in a linked worktree of a registered
/// checkout — nested under it or beside it, at its root or in a subdirectory
/// — carries the main checkout's registry `project_key`.
#[tokio::test]
async fn a_linked_worktree_session_carries_its_main_checkouts_project_key_10906() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = dir.path().join("registry");
    let web = checkout_10906(dir.path(), "web", Some("https://github.com/Acme/Web.git"));
    assert_eq!(register_10906(&reg, "web", &web).await, "acme/web");
    let nested = linked_worktree_10906(&web, &web.join(".claude/worktrees/wt"), "wt");
    let sibling = linked_worktree_10906(&web, &dir.path().join("web-wt"), "wt2");
    let cwds = [nested.join("sub"), nested.clone(), sibling.join("sub")];
    let cwds: Vec<&Path> = cwds.iter().map(PathBuf::as_path).collect();
    for (id, projects) in projects_for_10906(&dir, &reg, &cwds).await {
        assert_eq!(projects, [None, Some("acme/web".to_string())], "{id}");
    }
}

/// #10906 error arm: a worktree whose `gitdir:` names a missing admin dir
/// maps to no main checkout, so its events go out at once with no project,
/// never claimed by the registered checkout that encloses it.
#[tokio::test]
async fn a_worktree_with_a_broken_gitdir_sends_its_events_unattributed_10906() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reg = dir.path().join("registry");
    let web = checkout_10906(dir.path(), "web", Some("https://github.com/acme/web"));
    assert_eq!(register_10906(&reg, "web", &web).await, "acme/web");
    let broken = web.join(".claude/worktrees/gone");
    std::fs::create_dir_all(broken.join("sub")).expect("mkdir");
    std::fs::write(
        broken.join(".git"),
        "gitdir: /nonexistent/.git/worktrees/gone\n",
    )
    .expect("write");
    let cwds = [broken.join("sub")];
    let cwds: Vec<&Path> = cwds.iter().map(PathBuf::as_path).collect();
    for (id, projects) in projects_for_10906(&dir, &reg, &cwds).await {
        assert_eq!(projects, vec![None; 2], "{id}");
    }
}

/// #10906 regression: a project lookup stuck on the disk — a stale network
/// mount under the cwd — never holds the hook path. The hook that queues it
/// returns within the 6849 ingest bound, and a second session's hook does
/// not wait on it; the answer still lands once the lookup finishes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_project_lookup_never_holds_the_hook_path_10906() {
    const STALL: Duration = Duration::from_secs(2);
    let dir = tempfile::tempdir().expect("tempdir");
    let slow: projection::ProjectResolver = Arc::new(|cwd: &Path| {
        if cwd == Path::new("/stale") {
            std::thread::sleep(STALL);
            return Some("acme/stale".to_string());
        }
        Some("acme/live".to_string())
    });
    let state = Arc::new(DaemonState::new());
    let publisher = Arc::new(ActionPublisher::new(sock_in(&dir)).with_project_resolver(slow));
    assert!(state.action_events().attach(Arc::clone(&publisher)));
    let (a, b) = (SessionId::new(), SessionId::new());

    let hook_a = std::thread::spawn({
        let state = Arc::clone(&state);
        move || {
            let started = Instant::now();
            state.push_hook_event(pre_tool_use_in(a, Path::new("/stale")));
            started.elapsed()
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    state.push_hook_event(pre_tool_use_in(b, Path::new("/live")));
    let second = started.elapsed();
    let first = hook_a.join().expect("join");
    let waits = format!("first session {first:?}, second session {second:?}");
    assert!(first < STALL, "the hook waited on the lookup: {waits}");
    assert!(
        second < Duration::from_secs(1),
        "the second session's hook waited on the first one's lookup: {waits}"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while publisher.threader.lock().project_of(&a).is_none() {
        assert!(Instant::now() < deadline, "the slow lookup never landed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let threader = publisher.threader.lock();
    assert_eq!(threader.project_of(&a).as_deref(), Some("acme/stale"));
    assert_eq!(threader.project_of(&b).as_deref(), Some("acme/live"));
}

/// #10906 regression: a lookup requeued at [`MAX_LOOKUPS_IN_FLIGHT`] retries
/// from the session's first cwd. The session moves to another registered
/// checkout while the cap holds, and still lands its first cwd's key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_capped_project_lookup_retries_from_its_first_cwd_10906() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (gate, held) = std::sync::mpsc::channel::<()>();
    let held = Arc::new(std::sync::Mutex::new(held));
    let resolver: projection::ProjectResolver = Arc::new(move |cwd: &Path| {
        if let Ok(n) = cwd.strip_prefix("/held") {
            // Blocks until the test drops `gate`.
            let _ = held.lock().expect("gate").recv();
            return Some(format!("acme/held-{}", n.display()));
        }
        match cwd.to_str() {
            Some("/first") => Some("acme/first".to_string()),
            Some("/second") => Some("acme/second".to_string()),
            _ => None,
        }
    });
    let publisher = ActionPublisher::new(sock_in(&dir)).with_project_resolver(resolver);
    let held_sessions: Vec<SessionId> = (0..MAX_LOOKUPS_IN_FLIGHT)
        .map(|_| SessionId::new())
        .collect();
    for (i, s) in held_sessions.iter().enumerate() {
        publisher.publish(&pre_tool_use_in(
            *s,
            &Path::new("/held").join(i.to_string()),
        ));
    }
    let in_flight = || publisher.lookups.load(Ordering::SeqCst);
    assert_eq!(in_flight(), MAX_LOOKUPS_IN_FLIGHT, "the cap is full");

    let moved = SessionId::new();
    publisher.publish(&pre_tool_use_in(moved, Path::new("/first")));
    assert_eq!(in_flight(), MAX_LOOKUPS_IN_FLIGHT, "the 17th was requeued");
    assert_eq!(publisher.threader.lock().project_of(&moved), None);
    publisher.publish(&pre_tool_use_in(moved, Path::new("/second")));
    assert_eq!(publisher.threader.lock().project_of(&moved), None);

    drop(gate);
    publisher.lookups_settled().await;
    publisher.publish(&pre_tool_use_in(moved, Path::new("/second")));
    publisher.lookups_settled().await;
    let threader = publisher.threader.lock();
    for (i, s) in held_sessions.iter().enumerate() {
        let key = format!("acme/held-{i}");
        assert_eq!(threader.project_of(s).as_deref(), Some(key.as_str()));
    }
    assert_eq!(
        threader.project_of(&moved).as_deref(),
        Some("acme/first"),
        "the retry resolved from a later cwd, not the session's first"
    );
}
