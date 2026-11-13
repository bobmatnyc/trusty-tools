//! #10942, #10943: the MACHINE STATUS feed over real sockets, and how each
//! feed state draws on a ratatui buffer. Hermetic: stub sockets in a tempdir,
//! a test backend, no sleeps.

use ratatui::{Terminal, backend::TestBackend};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use trusty_common::console_metrics::machine_status::MachineStatus;
use trusty_common::console_rpc::metrics::HistorySnapshot;

use super::analyze_probe_tests::{DEADLINE, Peer, serve, socket_dir};
use super::console::{model_from_state, render_body};
use super::machine::{
    Fault, Feed, MachineFeeds, MachineUpdate, STATUS_INTERVAL, fetch_history, fetch_machine_status,
    spawn_poller,
};
use super::machine_cards::{machine_model, service_figures};
use super::services::ServiceId;
use super::state::HomeState;
use crate::tui::theme::Tone;

const GIB: u64 = 1 << 30;
const BARS: &str = "▁▂▃▄▅▆▇█";

/// One host sample; `pressure` is the CPU and overall level.
fn host(cpu: f64, pressure: &str) -> Value {
    json!({
        "cpu": {"usage_pct": cpu, "logical_cores": 16, "physical_cores": 16, "pressure": pressure},
        "memory": {"total_bytes": 128 * GIB, "used_bytes": 58 * GIB, "available_bytes": 70 * GIB,
            "usage_pct": 45.2, "swap_total_bytes": 4 * GIB, "swap_used_bytes": GIB, "pressure": "nominal"},
        "disks": {"aggregate_total_bytes": 7444 * GIB, "aggregate_available_bytes": 1629 * GIB,
            "aggregate_used_bytes": 5815 * GIB, "aggregate_usage_pct": 78.1, "pressure": "nominal",
            "mounts": []},
        "network": {"rx_bytes_per_sec": 1.2e6, "tx_bytes_per_sec": 0.3e6, "rx_total_bytes": 0,
            "tx_total_bytes": 0, "window_secs": 1.0},
        "overall_pressure": pressure,
        "sampled_at_unix": 1,
    })
}

/// A `MachineStatus` body around `host` at `schema`.
fn machine(host: Value, schema: u32) -> Value {
    json!({"host": host, "services": {"total": 0, "ok": 0, "degraded": 0, "error": 0,
        "services": []}, "schema_version": schema, "assembled_at_unix": 1})
}

/// A `HistorySnapshot` body: 40 host samples and a search ring.
fn history(schema: u32, search: Value) -> Value {
    let samples: Vec<Value> = (0..40)
        .map(|i| host(f64::from(i % 9) * 10.0, "nominal"))
        .collect();
    json!({"samples": samples, "service_samples": {"trusty-search": search},
        "sample_capacity": 600, "sample_interval_secs": 1, "schema_version": schema})
}

fn search_ring() -> Value {
    json!([
        {"id": "trusty-search", "status": "running", "cpu_pct": 2.0, "rss_bytes": 300 * (1 << 20)},
        {"id": "trusty-search", "status": "running", "cpu_pct": null, "rss_bytes": null},
        {"id": "trusty-search", "status": "running", "cpu_pct": 3.8, "rss_bytes": 1288490188u64},
    ])
}

/// A framed `result` reply, leaked so the stub can serve it as `'static`.
fn reply(result: Value) -> &'static [u8] {
    let frame = json!({"jsonrpc": "2.0", "id": 1, "result": result});
    Box::leak(format!("{frame}\n").into_bytes().into_boxed_slice())
}

fn ready_status() -> Feed<MachineStatus> {
    Feed::Ready(serde_json::from_value(machine(host(18.6, "nominal"), 1)).expect("status"))
}

fn ready_history(search: Value) -> Feed<HistorySnapshot> {
    Feed::Ready(serde_json::from_value(history(4, search)).expect("history"))
}

/// Draw the live home model for `feeds` at 160x30 and return the screen text.
fn screen(feeds: MachineFeeds) -> String {
    let state = HomeState {
        machine: feeds,
        ..HomeState::default()
    };
    let model = model_from_state(&state);
    let mut terminal = Terminal::new(TestBackend::new(160, 30)).expect("test backend");
    terminal
        .draw(|f| render_body(f, f.area(), &model))
        .expect("draw");
    let buffer = terminal.backend().buffer();
    let w = buffer.area.width as usize;
    buffer
        .content()
        .chunks(w)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>() + "\n")
        .collect()
}

async fn status_from(peer: Peer) -> Feed<MachineStatus> {
    let (_dir, socket) = socket_dir();
    serve(&socket, peer);
    fetch_machine_status(&socket, DEADLINE).await
}

#[tokio::test]
async fn machine_status_over_uds_maps_each_answer() {
    let ready = reply(json!({"ready": true, "machine": machine(host(18.6, "warning"), 1)}));
    match status_from(Peer::Reply(ready)).await {
        Feed::Ready(m) => assert!((m.host.cpu.usage_pct - 18.6).abs() < 0.01),
        other => panic!("want Ready, got {other:?}"),
    }
    let cold = reply(json!({"ready": false, "machine": null}));
    assert!(matches!(status_from(Peer::Reply(cold)).await, Feed::Cold));
    // #10942: a schema this build does not read is unknown, never numbers.
    let newer = reply(json!({"ready": true, "machine": machine(host(18.6, "nominal"), 2)}));
    assert!(matches!(
        status_from(Peer::Reply(newer)).await,
        Feed::Failed(Fault::Unknown(d)) if d.contains("schema v2")
    ));
    let hollow = reply(json!({"ready": true, "machine": null}));
    assert!(matches!(
        status_from(Peer::Reply(hollow)).await,
        Feed::Failed(Fault::Unknown(_))
    ));
}

#[tokio::test]
async fn history_over_uds_maps_each_answer() {
    let cases = [
        (history(4, search_ring()), "ready"),
        (history(99, search_ring()), "unknown"),
        // Every field defaults, so a foreign body decodes as schema 0.
        (json!({"something": "else"}), "unknown"),
    ];
    for (body, want) in cases {
        let (_dir, socket) = socket_dir();
        serve(&socket, Peer::Reply(reply(body)));
        let feed = fetch_history(&socket, DEADLINE).await;
        let got = match &feed {
            Feed::Ready(h) => {
                assert_eq!(h.samples.len(), 40);
                "ready"
            }
            Feed::Failed(Fault::Unknown(_)) => "unknown",
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(got, want);
    }
}

#[tokio::test]
async fn a_console_fault_maps_to_its_own_kind() {
    // Absent: no listener at all.
    let (_dir, socket) = socket_dir();
    assert!(matches!(
        fetch_machine_status(&socket, DEADLINE).await,
        Feed::Failed(Fault::Absent(_))
    ));
    let error: &[u8] =
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"code\":-32601,\"message\":\"method not found\"}}\n";
    assert!(matches!(
        status_from(Peer::Reply(error)).await,
        Feed::Failed(Fault::Error(d)) if d.contains("method not found")
    ));
    assert!(matches!(
        status_from(Peer::Reply(b"not json\n")).await,
        Feed::Failed(Fault::Unknown(_))
    ));
    // An unknown enum variant fails the decode: unknown, not a default.
    let variant = reply(json!({"ready": true, "machine": machine(host(1.0, "meltdown"), 1)}));
    assert!(matches!(
        status_from(Peer::Reply(variant)).await,
        Feed::Failed(Fault::Unknown(_))
    ));
    assert!(matches!(
        status_from(Peer::HangUp).await,
        Feed::Failed(Fault::Error(_))
    ));
    // Slow: the peer holds the connection past the deadline.
    let (_dir, socket) = socket_dir();
    serve(&socket, Peer::Hold);
    let slow = fetch_machine_status(&socket, std::time::Duration::from_millis(200)).await;
    assert!(matches!(slow, Feed::Failed(Fault::Timeout)), "{slow:?}");
}

#[test]
fn a_ready_feed_fills_every_card_with_live_values() {
    let text = screen(MachineFeeds {
        status: ready_status(),
        history: ready_history(search_ring()),
    });
    for want in [
        " NOMINAL ",
        "18.6%",
        "16 logical / 16 physical",
        "58.0 / 128.0 GiB",
        "78.1% used",
        "↓ 1.2  ↑ 0.3 MB/s",
    ] {
        assert!(text.contains(want), "{want}: {text}");
    }
    let machine_rows: String = text.lines().take(8).collect();
    assert!(
        machine_rows.chars().any(|c| BARS.contains(c)),
        "card sparklines: {text}"
    );
}

#[test]
fn every_fault_draws_its_own_label_and_no_values() {
    let faults = [
        (
            Fault::Absent("nothing listens".into()),
            " CONSOLE ABSENT ",
            Tone::Muted,
        ),
        (Fault::Timeout, " TIMEOUT ", Tone::Warn),
        (Fault::Error("code -1: boom".into()), " ERROR ", Tone::Err),
        (
            Fault::Unknown("schema v9".into()),
            " UNKNOWN ",
            Tone::Degraded,
        ),
    ];
    for (fault, word, tone) in faults {
        assert_eq!(fault.label().1, tone);
        let feeds = MachineFeeds {
            status: Feed::Failed(fault.clone()),
            history: Feed::Failed(fault),
        };
        let text = screen(feeds);
        assert!(text.contains(word), "{word}: {text}");
        let cards: String = text.lines().take(8).collect();
        assert!(!cards.contains('%'), "no value is drawn: {text}");
        assert!(cards.contains("unknown"), "values read unknown: {text}");
        assert!(
            !text.chars().any(|c| BARS.contains(c)),
            "no sparkline: {text}"
        );
    }
    // Live cards, broken history: the graph rows name the history's fault.
    let text = screen(MachineFeeds {
        status: ready_status(),
        history: Feed::Failed(Fault::Timeout),
    });
    assert!(text.contains("18.6%"));
    assert!(text.contains("history timeout"), "{text}");
}

#[test]
fn a_cold_console_reads_warming_up() {
    let feeds = MachineFeeds {
        status: Feed::Cold,
        history: Feed::Pending,
    };
    let machine = machine_model(&feeds).expect("a labelled section");
    assert_eq!(machine.badge, ("warming up".to_string(), Tone::Info));
    let text = screen(feeds);
    assert!(text.contains(" WARMING UP "), "{text}");
    assert!(text.contains("waiting for console.history"), "{text}");
    assert!(machine_model(&MachineFeeds::default()).is_none());
}

/// The regression proof: a feed that keeps an earlier Ready on a later
/// failure leaves 18.6% on screen as if live.
#[test]
fn a_later_error_replaces_earlier_data() {
    let mut feeds = MachineFeeds::default();
    feeds.apply(MachineUpdate::Status(ready_status()));
    feeds.apply(MachineUpdate::History(ready_history(search_ring())));
    assert!(screen(feeds.clone()).contains("18.6%"));
    feeds.apply(MachineUpdate::Status(Feed::Failed(Fault::Timeout)));
    feeds.apply(MachineUpdate::History(Feed::Failed(Fault::Error(
        "boom".into(),
    ))));
    let text = screen(feeds);
    assert!(!text.contains("18.6%"), "stale value drawn as live: {text}");
    assert!(!text.contains("1.2 GiB"), "stale service value: {text}");
    assert!(text.contains(" TIMEOUT "), "{text}");
    assert!(text.contains("history error"), "{text}");
}

#[test]
fn service_rows_take_cpu_and_memory_from_service_samples() {
    let history = ready_history(search_ring());
    let figures = service_figures(&history, ServiceId::Search.id());
    assert_eq!(figures.cpu.as_deref(), Some("3.8"));
    assert_eq!(figures.mem.as_deref(), Some("1.2 GiB"));
    assert_eq!(figures.cpu_history, Some(vec![Some(20), None, Some(38)]));
    // A service the history has no ring for: empty series, no values.
    let other = service_figures(&history, ServiceId::Memory.id());
    assert_eq!((other.cpu, other.cpu_history), (None, Some(vec![])));
    // Not ready: nothing at all.
    assert_eq!(
        service_figures(&Feed::Pending, ServiceId::Search.id()),
        Default::default()
    );
    let text = screen(MachineFeeds {
        status: ready_status(),
        history,
    });
    let search = text
        .lines()
        .find(|l| l.contains("Search"))
        .expect("a Search row");
    assert!(
        search.contains("3.8") && search.contains("1.2 GiB"),
        "{search}"
    );
    assert!(search.chars().any(|c| BARS.contains(c)), "{search}");
    assert!(
        text.contains("no samples yet"),
        "rows without a ring: {text}"
    );
}

#[test]
fn an_unknown_service_status_is_a_gap_not_a_value() {
    let ring = json!([
        {"id": "trusty-search", "status": "running", "cpu_pct": 2.0, "rss_bytes": 1},
        {"id": "trusty-search", "status": "hibernating", "cpu_pct": 9.0, "rss_bytes": 2},
    ]);
    let figures = service_figures(&ready_history(ring), ServiceId::Search.id());
    assert_eq!(figures.cpu.as_deref(), Some("unknown"));
    assert_eq!(figures.mem.as_deref(), Some("unknown"));
    assert_eq!(figures.cpu_history, Some(vec![Some(20), None]));
    assert_eq!(figures.mem_history, Some(vec![Some(1), None]));
}

#[tokio::test(start_paused = true)]
async fn the_poller_runs_off_the_ui_loop_and_stops_with_the_screen() {
    let (tx, mut rx) = mpsc::channel(4);
    // A hung call on its own task must not hold back the other method.
    spawn_poller(
        std::future::pending::<MachineUpdate>,
        tx.clone(),
        STATUS_INTERVAL,
    );
    spawn_poller(
        || async { MachineUpdate::History(Feed::Cold) },
        tx,
        STATUS_INTERVAL,
    );
    for _ in 0..2 {
        let update = rx.recv().await.expect("an update");
        assert!(matches!(update, MachineUpdate::History(Feed::Cold)));
    }
    drop(rx);
}
