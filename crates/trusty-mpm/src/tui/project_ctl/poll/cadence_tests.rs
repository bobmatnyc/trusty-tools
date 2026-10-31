//! #10476: a normal refresh tick serves the cached Activity pane and skips
//! the health probe.
//!
//! Why: `/activity` costs a tmux capture on the daemon, and `/health` a round
//! trip; the poll asked for both on every 1 s tick.
//! What: a stub daemon counts `/health`, fleet and `/activity` requests. One
//! test runs three back-to-back polls with one session selected; another runs
//! the real loop and counts health probes against polls; a third shows a
//! failed fleet call on a poll that skipped the probe still reads as down.
//! Test: this is the test module.

use std::future::IntoFuture;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::project_ctl_poll_daemon;
use crate::client::DaemonClient;
use crate::tui::project_ctl::state::ProjectCtlState;

/// Request counts the stub daemon records.
#[derive(Default)]
struct Hits {
    health: AtomicUsize,
    fleet: AtomicUsize,
    activity: AtomicUsize,
}

/// Serve the five routes one poll reads; count health, fleet and activity hits.
async fn stub_daemon(hits: Arc<Hits>) -> String {
    use axum::routing::get;
    let json = |v: serde_json::Value| move || std::future::ready(axum::Json(v.clone()));
    let activity = serde_json::json!({
        "raw_pane": "line one\nline two",
        "runtime_active": true,
        "state": "working",
        "summary": "editing",
        "confidence": 0.9,
        "cache_hit": false,
        "input_tokens": 0,
        "output_tokens": 0,
        "latency_ms": 0,
        "total_input_tokens": 0,
        "total_output_tokens": 0
    });
    let app = axum::Router::new()
        .route(
            "/health",
            get({
                let hits = hits.clone();
                move || {
                    hits.health.fetch_add(1, Ordering::SeqCst);
                    std::future::ready("ok")
                }
            }),
        )
        .route(
            "/api/v1/projects",
            get(json(serde_json::json!({"projects": [{
                "name": "widget",
                "repo_url": "https://github.com/acme/widget",
                "default_branch": "main"
            }]}))),
        )
        .route(
            "/api/v1/sessions/managed/fleet",
            get({
                let hits = hits.clone();
                let body = json(serde_json::json!({"projects": [{
                    "project_name": "widget",
                    "repo_url": "https://github.com/acme/widget",
                    "sessions": [{"id": "s-10476", "name": "widget-1", "state": "active"}]
                }]}));
                move || {
                    hits.fleet.fetch_add(1, Ordering::SeqCst);
                    body()
                }
            }),
        )
        .route(
            "/api/v1/sessions/managed/{id}/activity",
            get(move || {
                hits.activity.fetch_add(1, Ordering::SeqCst);
                std::future::ready(axum::Json(activity.clone()))
            }),
        )
        .route(
            "/api/v1/projects/{project}/deliverables",
            get(json(serde_json::json!({"deliverables": []}))),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub daemon");
    let addr = listener.local_addr().expect("stub addr");
    tokio::spawn(axum::serve(listener, app).into_future());
    format!("http://{addr}")
}

#[tokio::test]
async fn normal_tick_makes_no_activity_call_10476() {
    let hits = Arc::new(Hits::default());
    let mut client = DaemonClient::new(stub_daemon(hits.clone()).await);
    let mut state = ProjectCtlState::default();

    for _ in 0..3 {
        project_ctl_poll_daemon(&mut state, &mut client).await;
    }

    assert!(state.daemon_reachable, "the stub daemon must read as up");
    assert_eq!(
        state.activity.as_ref().map(|a| a.session_id.as_str()),
        Some("s-10476"),
        "the first poll fills the Activity pane"
    );
    assert_eq!(
        hits.activity.load(Ordering::SeqCst),
        1,
        "three polls within the refresh window must make one /activity call (a tmux capture), not one per tick"
    );
}

/// Quits once the stub has served `after` fleet requests (one per poll).
struct QuitAfterPolls {
    hits: Arc<Hits>,
    after: usize,
}

impl crate::tui::project_ctl::drive::EventSource for QuitAfterPolls {
    fn next_event(
        &mut self,
        timeout: std::time::Duration,
    ) -> std::io::Result<Option<crossterm::event::Event>> {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        if self.hits.fleet.load(Ordering::SeqCst) >= self.after {
            return Ok(Some(Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
            ))));
        }
        std::thread::sleep(timeout.min(std::time::Duration::from_millis(5)));
        Ok(None)
    }
}

/// #10476: the loop probes `/health` on the first poll and every
/// `HEALTH_EVERY`th one, not on every poll.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_checked_every_nth_tick() {
    const N: usize = super::tick::HEALTH_EVERY as usize;
    let polls = 2 * N + 1;
    let hits = Arc::new(Hits::default());
    let mut client = DaemonClient::new(stub_daemon(hits.clone()).await);
    let mut events = QuitAfterPolls {
        hits: hits.clone(),
        after: polls,
    };

    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        crate::tui::project_ctl::drive::drive(
            &mut |_: &mut ProjectCtlState| Ok(()),
            &mut events,
            super::tick::fetch_tick,
            &mut client,
            ProjectCtlState::default(),
            std::time::Duration::ZERO,
            &mut crate::tui::project_ctl::trace::TuiTrace::disabled(),
        ),
    )
    .await
    .expect("the loop quits once the stub served the polls")
    .expect("drive returns Ok on q");

    // A poll can start in the turn before `q` is read, so the loop may run
    // one poll past `polls`; that poll (index 2N+1) is never due.
    let ran = hits.fleet.load(Ordering::SeqCst);
    assert!(
        (polls..=polls + 1).contains(&ran),
        "ran {ran} polls, wanted {polls}"
    );
    assert_eq!(
        hits.health.load(Ordering::SeqCst),
        ran.div_ceil(N),
        "{ran} polls with HEALTH_EVERY = {N} probe health on polls 1, {} and {} only",
        N + 1,
        2 * N + 1
    );
}

/// #10476: a poll that skips the health probe still marks the daemon
/// unreachable when its fleet call fails, and a later good poll recovers.
#[tokio::test]
async fn fleet_error_without_health_check_marks_unreachable_10476() {
    use super::tick::{PollRequest, apply_tick, fetch_tick};

    let hits = Arc::new(Hits::default());
    let live = DaemonClient::new(stub_daemon(hits.clone()).await);
    let mut state = ProjectCtlState::default();
    let skip = PollRequest::default();
    assert!(!skip.check_health);

    let up = fetch_tick(
        live.clone(),
        PollRequest {
            check_health: true,
            ..PollRequest::default()
        },
    )
    .await;
    apply_tick(&mut state, up.result);
    assert!(state.daemon_reachable, "the stub daemon reads as up");

    let dead = DaemonClient::new(crate::test_support::dead_loopback_url());
    let down = fetch_tick(dead, skip.clone()).await;
    assert!(
        !down.timing.calls.iter().any(|(name, _)| *name == "health"),
        "this poll must skip the probe"
    );
    apply_tick(&mut state, down.result);
    assert!(
        !state.daemon_reachable,
        "a refused fleet call reads as down"
    );
    assert!(state.projects.is_empty(), "a down daemon clears the lists");

    let back = fetch_tick(live, skip).await;
    apply_tick(&mut state, back.result);
    assert!(state.daemon_reachable, "a later good poll recovers");
    assert_eq!(state.projects.len(), 1);
    assert_eq!(
        hits.health.load(Ordering::SeqCst),
        1,
        "only the first poll probed"
    );
}
