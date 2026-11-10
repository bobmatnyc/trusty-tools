//! Tests for the console home screen (#10491).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;
use tokio::sync::mpsc;
use trusty_common::console_metrics::{ConsoleMetricsReport, ServiceHealth};

use super::probe::{
    Observation, classify, fetch_metrics, on_demand_when_idle, spawn_health_poller,
    transport_failure, with_deadline,
};
use super::render::{metric_lines, render, service_lines};
use super::services::ServiceId;
use super::state::{
    HealthUpdate, HomeAction, HomeState, METRICS_TTL, MetricsState, MetricsUpdate, TileHealth,
    View, handle_key,
};
use super::{TuiEntry, tui_entry};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn select(state: &mut HomeState, id: ServiceId) {
    state.selected = ServiceId::ALL
        .iter()
        .position(|&s| s == id)
        .expect("id is in the roster");
}

fn up(state: &mut HomeState, id: ServiceId) {
    state.apply_health(HealthUpdate {
        id,
        health: TileHealth::Up {
            version: Some("1.2.3".into()),
        },
    });
}

fn report() -> ConsoleMetricsReport {
    ConsoleMetricsReport {
        service_id: "trusty-search".into(),
        display_name: "Search".into(),
        version: "0.57.1".into(),
        status: ServiceHealth::Ok,
        metrics: json!({"index_count": 3, "indexes": [1, 2], "build": {"a": 1}}),
        metrics_schema_version: 1,
        collected_at_unix: None,
    }
}

/// The test that fails without #10491: `tm tui` used to open the projects
/// view directly.
#[test]
fn tm_tui_opens_on_the_console_home_screen() {
    assert_eq!(tui_entry(false), TuiEntry::Home);
    assert_eq!(tui_entry(true), TuiEntry::SinglePane);
}

/// The test that fails without #10476: Search used to be first and selected.
#[test]
fn home_opens_with_mpm_first_and_selected() {
    assert_eq!(ServiceId::ALL[0], ServiceId::Mpm);
    let state = HomeState::default();
    assert_eq!(state.selected_id(), ServiceId::Mpm);
    assert_eq!(state.tiles[0].0, ServiceId::Mpm, "MPM tile renders first");
}

#[test]
fn roster_lists_mpm_first_then_the_console_order() {
    let ids: Vec<&str> = ServiceId::ALL.iter().map(|s| s.id()).collect();
    assert_eq!(
        ids,
        [
            "trusty-mpm",
            "trusty-search",
            "trusty-memory",
            "trusty-analyze",
            "trusty-review",
            "trusty-agents",
            "trusty-events",
            "trusty-console",
        ]
    );
    let state = HomeState::default();
    assert!(state.tiles.iter().all(|(_, h)| *h == TileHealth::Pending));
}

#[test]
fn arrows_move_the_selection_within_the_grid() {
    let mut state = HomeState::default();
    let now = Instant::now();
    handle_key(&mut state, key(KeyCode::Left), now);
    assert_eq!(state.selected, 0, "clamped at the first tile");
    handle_key(&mut state, key(KeyCode::Down), now);
    assert_eq!(state.selected, 4, "down moves one row");
    handle_key(&mut state, key(KeyCode::Right), now);
    handle_key(&mut state, key(KeyCode::Down), now);
    assert_eq!(state.selected, 7, "clamped at the last tile");
    handle_key(&mut state, key(KeyCode::Up), now);
    assert_eq!(state.selected, 3);
}

#[test]
fn enter_on_mpm_opens_projects() {
    let mut state = HomeState::default();
    select(&mut state, ServiceId::Mpm);
    let action = handle_key(&mut state, key(KeyCode::Enter), Instant::now());
    assert_eq!(action, Some(HomeAction::OpenProjects));
    assert_eq!(state.view, View::Home, "the projects view owns the screen");
}

#[test]
fn enter_on_search_opens_its_view_and_esc_returns() {
    let mut state = HomeState::default();
    select(&mut state, ServiceId::Search);
    up(&mut state, ServiceId::Search);
    let action = handle_key(&mut state, key(KeyCode::Enter), Instant::now());
    assert_eq!(action, Some(HomeAction::FetchMetrics(ServiceId::Search)));
    assert_eq!(state.view, View::Service(ServiceId::Search));
    handle_key(&mut state, key(KeyCode::Esc), Instant::now());
    assert_eq!(state.view, View::Home);
    assert!(!state.should_exit);
}

#[test]
fn q_quits_from_any_view() {
    for view in [View::Home, View::Service(ServiceId::Memory)] {
        let mut state = HomeState {
            view,
            ..HomeState::default()
        };
        handle_key(&mut state, key(KeyCode::Char('q')), Instant::now());
        assert!(state.should_exit);
    }
}

#[test]
fn metrics_fetch_is_throttled_by_the_ttl() {
    let mut state = HomeState::default();
    up(&mut state, ServiceId::Search);
    let t0 = Instant::now();
    assert!(state.request_metrics(ServiceId::Search, t0));
    assert!(
        !state.request_metrics(ServiceId::Search, t0),
        "in flight: no second spawn"
    );
    state.apply_metrics(
        MetricsUpdate {
            id: ServiceId::Search,
            result: Ok(report()),
        },
        t0,
    );
    assert!(!state.request_metrics(ServiceId::Search, t0 + Duration::from_secs(1)));
    assert!(state.request_metrics(ServiceId::Search, t0 + METRICS_TTL));
}

#[test]
fn a_down_service_shows_unavailable_without_a_fetch() {
    let mut state = HomeState::default();
    state.apply_health(HealthUpdate {
        id: ServiceId::Memory,
        health: TileHealth::Down {
            reason: "no socket".into(),
        },
    });
    select(&mut state, ServiceId::Memory);
    let action = handle_key(&mut state, key(KeyCode::Enter), Instant::now());
    assert_eq!(action, None, "a stopped service is not spawned");
    assert!(matches!(
        state.metrics.get(&ServiceId::Memory),
        Some(MetricsState::Unavailable { reason, .. }) if reason.contains("down")
    ));
}

#[test]
fn classify_maps_each_observation_to_a_tile_state() {
    let cases = [
        (
            Observation::Healthy(Some("1".into())),
            true,
            TileHealth::Up {
                version: Some("1".into()),
            },
        ),
        (
            Observation::Unhealthy("x".into()),
            true,
            TileHealth::Degraded { reason: "x".into() },
        ),
        (
            Observation::Silent("gone".into()),
            true,
            TileHealth::Down {
                reason: "gone".into(),
            },
        ),
        (
            Observation::Silent("gone".into()),
            false,
            TileHealth::Absent,
        ),
        (
            Observation::NotListening("idle".into()),
            true,
            TileHealth::Down {
                reason: "idle".into(),
            },
        ),
        (
            Observation::NotListening("idle".into()),
            false,
            TileHealth::Absent,
        ),
        (Observation::OnDemand, true, TileHealth::OnDemand),
        (Observation::OnDemand, false, TileHealth::Absent),
        (
            Observation::Healthy(None),
            false,
            TileHealth::Up { version: None },
        ),
    ];
    for (observation, installed, want) in cases {
        assert_eq!(classify(observation, installed), want);
    }
}

/// #10476: trusty-analyze is on demand, so an installed analyze with nothing
/// listening is idle, not down. Every other service keeps not-listening as
/// Down, and a `Silent` failure (an unresolvable socket path included) is
/// Down for analyze too.
#[test]
fn an_idle_analyze_reads_as_on_demand_not_down() {
    let idle = || Observation::NotListening("no socket".into());
    let seen = |id| classify(on_demand_when_idle(id, idle()), true);
    assert_eq!(seen(ServiceId::Analyze), TileHealth::OnDemand);
    for id in [ServiceId::Search, ServiceId::Memory, ServiceId::Mpm] {
        assert_eq!(
            seen(id),
            TileHealth::Down {
                reason: "no socket".into()
            },
            "{id:?}"
        );
    }
    assert_eq!(
        classify(on_demand_when_idle(ServiceId::Analyze, idle()), false),
        TileHealth::Absent,
        "an analyze that is not installed is still absent"
    );
    let unresolved = Observation::Silent("analyze socket unresolved".into());
    assert_eq!(
        classify(on_demand_when_idle(ServiceId::Analyze, unresolved), true),
        TileHealth::Down {
            reason: "analyze socket unresolved".into()
        },
        "an unresolvable analyze socket path is down"
    );
}

/// #10476: only a dial that found no listener is `NotListening`; every other
/// transport failure is `Silent`, so an on-demand analyze still reads Down.
#[test]
fn transport_failures_split_not_listening_from_broken() {
    use std::io::{Error, ErrorKind};
    use std::path::PathBuf;
    use trusty_common::uds::{UdsRpcError, UdsSecurityError};

    let path = || PathBuf::from("/run/analyze.sock");
    let dial = |source| UdsRpcError::Dial {
        path: path(),
        source,
    };
    let connect = |kind| {
        dial(UdsSecurityError::Connect {
            path: path(),
            source: Error::from(kind),
        })
    };
    let stat = |kind| {
        dial(UdsSecurityError::StatForConnect {
            path: path(),
            source: Error::from(kind),
        })
    };
    let retried = |last| UdsRpcError::ConnectRetriesExhausted {
        path: path(),
        attempts: 3,
        source: Box::new(last),
    };
    let not_listening = [
        stat(ErrorKind::NotFound),
        connect(ErrorKind::NotFound),
        connect(ErrorKind::ConnectionRefused),
        retried(connect(ErrorKind::ConnectionRefused)),
    ];
    for err in not_listening {
        assert!(
            matches!(transport_failure(&err), Observation::NotListening(_)),
            "{err}"
        );
    }
    let decode_error = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
    let broken = [
        stat(ErrorKind::PermissionDenied),
        connect(ErrorKind::PermissionDenied),
        dial(UdsSecurityError::UntrustedSocket {
            path: path(),
            reason: "socket is mode 0644, not 0600".into(),
        }),
        retried(connect(ErrorKind::PermissionDenied)),
        UdsRpcError::Write {
            path: path(),
            source: Error::from(ErrorKind::BrokenPipe),
        },
        UdsRpcError::Read {
            path: path(),
            source: Error::from(ErrorKind::Other),
        },
        UdsRpcError::HalfClose {
            path: path(),
            source: Error::from(ErrorKind::Other),
        },
        UdsRpcError::NoResponse { path: path() },
        UdsRpcError::FrameTooLarge {
            path: path(),
            limit: 16,
        },
        UdsRpcError::Decode {
            path: path(),
            source: decode_error,
        },
        UdsRpcError::Timeout {
            path: path(),
            timeout: Duration::from_millis(5),
        },
    ];
    for err in broken {
        let observed = transport_failure(&err);
        assert!(matches!(observed, Observation::Silent(_)), "{err}");
        assert!(
            matches!(
                classify(on_demand_when_idle(ServiceId::Analyze, observed), true),
                TileHealth::Down { .. }
            ),
            "{err}"
        );
    }
}

/// #10476: an analyze that answers health is still Up, and one that answers
/// badly is still Degraded.
#[test]
fn an_answering_analyze_still_reads_as_up() {
    let healthy = Observation::Healthy(Some("0.9.0".into()));
    assert_eq!(
        classify(on_demand_when_idle(ServiceId::Analyze, healthy), true),
        TileHealth::Up {
            version: Some("0.9.0".into())
        }
    );
    let unhealthy = Observation::Unhealthy("x".into());
    assert_eq!(
        classify(on_demand_when_idle(ServiceId::Analyze, unhealthy), true),
        TileHealth::Degraded { reason: "x".into() }
    );
}

#[tokio::test]
async fn a_probe_past_its_deadline_reads_as_timeout() {
    let hung = std::future::pending::<Observation>();
    let health = with_deadline(hung, true, Duration::from_millis(20)).await;
    assert_eq!(health, TileHealth::Timeout);
}

/// The poll runs on spawned tasks, not on the draw path: a probe that never
/// returns leaves the other tiles' updates flowing.
#[tokio::test]
async fn a_hung_probe_does_not_hold_back_the_other_tiles() {
    let (tx, mut rx) = mpsc::channel(8);
    spawn_health_poller(
        &[ServiceId::Search, ServiceId::Memory],
        |id| async move {
            if id == ServiceId::Search {
                std::future::pending::<()>().await;
            }
            TileHealth::Up { version: None }
        },
        tx,
        Duration::from_secs(60),
    );
    let update = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("memory's update arrives while search hangs")
        .expect("channel open");
    assert_eq!(update.id, ServiceId::Memory);
}

#[tokio::test]
async fn a_service_without_console_metrics_is_refused_without_a_spawn() {
    let update = fetch_metrics(ServiceId::Events, Duration::from_millis(50)).await;
    assert!(update.result.is_err());
}

#[test]
fn home_render_draws_every_tile() {
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).expect("test backend");
    let mut state = HomeState::default();
    up(&mut state, ServiceId::Search);
    state.apply_health(HealthUpdate {
        id: ServiceId::Agents,
        health: TileHealth::Absent,
    });
    terminal
        .draw(|f| render(f, &state))
        .expect("draw the home screen");
    let text: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    for id in ServiceId::ALL {
        assert!(text.contains(id.display_name()), "missing {id:?}");
    }
    assert!(text.contains("not installed"));
    assert!(text.contains("probing"));
}

#[test]
fn service_view_shows_unavailable_metrics() {
    let mut state = HomeState::default();
    state.apply_metrics(
        MetricsUpdate {
            id: ServiceId::Analyze,
            result: Err("timed out after 10s".into()),
        },
        Instant::now(),
    );
    let lines: Vec<String> = service_lines(ServiceId::Analyze, &state)
        .iter()
        .map(|l| l.to_string())
        .collect();
    assert!(
        lines
            .iter()
            .any(|l| l == "unavailable: timed out after 10s")
    );
    let metrics = metric_lines(&report().metrics);
    assert!(metrics.contains(&"  index_count: 3".to_string()));
    assert!(metrics.contains(&"  indexes: [2 items]".to_string()));
}

/// Row `y` of a drawn `TestBackend` buffer as text.
fn buffer_row(terminal: &Terminal<TestBackend>, y: u16) -> String {
    let buffer = terminal.backend().buffer();
    (0..buffer.area.width)
        .map(|x| buffer[(x, y)].symbol())
        .collect()
}

// #10476: version in tui header (Bob 11-05)
#[test]
fn home_header_shows_tm_version_at_80_columns() {
    let version = format!("tm {}", env!("CARGO_PKG_VERSION"));
    let mut state = HomeState::default();
    for view in [View::Home, View::Service(ServiceId::Search)] {
        state.view = view;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test backend");
        terminal
            .draw(|f| render(f, &state))
            .expect("draw the home screen");
        let top = buffer_row(&terminal, 0);
        assert!(
            top.contains(&version),
            "{view:?} header lacks {version}: {top:?}"
        );
        let title = match view {
            View::Home => "trusty console — services".to_string(),
            View::Service(id) => format!("trusty console › {}", id.id()),
        };
        assert!(top.starts_with(&title), "{view:?} title displaced: {top:?}");
    }
}

// #10942: the home screen draws in the console's dark palette. Literal RGB on
// purpose: the test must compile, and fail, against the pre-theme renderer.
#[test]
fn home_tile_title_and_background_use_theme_roles() {
    use ratatui::style::Color;
    let mut terminal = Terminal::new(TestBackend::new(120, 24)).expect("test backend");
    let state = HomeState::default();
    terminal
        .draw(|f| render(f, &state))
        .expect("draw the home screen");
    let buffer = terminal.backend().buffer();
    // The selected (first) tile's top-left border corner: accent on background.
    let corner = &buffer[(0, 1)];
    assert_eq!(corner.fg, Color::Rgb(0xd9, 0x77, 0x42), "accent border");
    assert_eq!(
        corner.bg,
        Color::Rgb(0x20, 0x16, 0x12),
        "console background"
    );
    // The version label at the header's right edge is muted text.
    let version = &buffer[(119, 0)];
    assert_eq!(
        version.fg,
        Color::Rgb(0xa5, 0x8a, 0x6b),
        "muted version label"
    );
}
