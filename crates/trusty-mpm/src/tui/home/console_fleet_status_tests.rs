//! #10966: the Console tile reads `console.fleet_status` over the console's
//! own socket (#10902). No HTTP: an absent or refused socket is the closed
//! console's resting state, never a fallback dial.

use std::path::Path;
use std::time::Duration;

use trusty_common::console_rpc::{CONSOLE_SERVICE, METHOD_FLEET_STATUS};

use super::analyze_probe_tests::{DEADLINE, Peer, serve, socket_dir};
use super::console::{model_from_state, status_of};
use super::probe::{classify, on_demand_when_idle, probe_service_at, uds_fleet_status};
use super::services::{ProbeKind, ServiceId, binary_on_path};
use super::state::{HealthUpdate, HomeState, TileHealth};
use crate::tui::theme::Tone;

/// A `console.fleet_status` answer whose own row is the console at 0.7.0.
const FLEET: &[u8] = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ready\":true,\"overlaid\":true,\"services\":[{\"id\":\"trusty-search\",\"display_name\":\"Trusty Search\",\"status\":\"available\"},{\"id\":\"trusty-console\",\"display_name\":\"Trusty Console\",\"status\":\"running\",\"version\":\"0.7.0\",\"lifecycle\":\"daemon\",\"cpu_pct\":null,\"rss_bytes\":null}]}}\n";

/// The Console tile for one probe of `socket`, installed.
async fn console_tile(socket: &Path) -> TileHealth {
    let observed = uds_fleet_status(socket, METHOD_FLEET_STATUS, DEADLINE).await;
    classify(on_demand_when_idle(ServiceId::Console, observed), true)
}

/// The regression proof: with Console mapped to `NoUdsEndpoint` the tile
/// never dials, so it reads "no UDS endpoint" instead of the stub's answer.
#[tokio::test]
async fn the_console_row_reads_running_with_its_version_from_fleet_status() {
    assert_eq!(
        ServiceId::Console.probe(),
        ProbeKind::UdsFleetStatus(METHOD_FLEET_STATUS)
    );
    let (_dir, socket) = socket_dir();
    serve(&socket, Peer::Reply(FLEET));
    let dialled = socket.clone();
    let health = probe_service_at(ServiceId::Console, move || Ok(dialled), DEADLINE).await;
    assert_eq!(
        health,
        TileHealth::Up {
            version: Some("0.7.0".into())
        }
    );
    assert_eq!(status_of(&health), ("running".to_string(), Tone::Ok));

    let mut state = HomeState::default();
    state.apply_health(HealthUpdate {
        id: ServiceId::Console,
        health,
    });
    let model = model_from_state(&state);
    let row = model
        .rows
        .iter()
        .find(|r| r.name == ServiceId::Console.display_name())
        .expect("a Console row");
    assert_eq!(row.version, "0.7.0");
    assert_eq!(row.status.0, "running");
    // S6 (#10942, #10943): machine cards and history stay empty here.
    assert!(model.machine.is_none());
    assert!(row.cpu_history.is_none() && row.mem_history.is_none());
}

#[tokio::test]
async fn an_absent_or_refused_console_socket_reads_as_on_demand_or_absent() {
    let want = if binary_on_path(ServiceId::Console.binary()) {
        TileHealth::OnDemand
    } else {
        TileHealth::Absent
    };
    let (_dir, socket) = socket_dir();
    let dialled = socket.clone();
    let absent = probe_service_at(ServiceId::Console, move || Ok(dialled), DEADLINE).await;
    assert_eq!(absent, want, "absent socket file");
    assert_eq!(console_tile(&socket).await, TileHealth::OnDemand);

    // A socket file whose listener is gone: connect is refused.
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind stub");
    drop(listener);
    assert!(socket.exists(), "the stale socket file stays behind");
    let dialled = socket.clone();
    let refused = probe_service_at(ServiceId::Console, move || Ok(dialled), DEADLINE).await;
    assert_eq!(refused, want, "refused connect");
    assert_eq!(console_tile(&socket).await, TileHealth::OnDemand);
}

#[tokio::test]
async fn a_broken_console_socket_reads_as_down() {
    let cases: [(&str, &[u8]); 3] = [
        ("malformed JSON", b"not json\n"),
        (
            "not a fleet status",
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"services\":7}}\n",
        ),
        ("no result", b"{\"jsonrpc\":\"2.0\",\"id\":1}\n"),
    ];
    for (case, reply) in cases {
        let (_dir, socket) = socket_dir();
        serve(&socket, Peer::Reply(reply));
        let tile = console_tile(&socket).await;
        assert!(
            matches!(tile, TileHealth::Down { .. }),
            "{case}: want Down, got {tile:?}"
        );
    }
    let (_dir, socket) = socket_dir();
    serve(&socket, Peer::HangUp);
    let tile = console_tile(&socket).await;
    assert!(matches!(tile, TileHealth::Down { .. }), "hang-up: {tile:?}");
}

#[tokio::test]
async fn a_cold_or_degraded_console_answer_maps_to_up_or_degraded() {
    let (_dir, socket) = socket_dir();
    serve(
        &socket,
        Peer::Reply(
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ready\":false,\"services\":[]}}\n",
        ),
    );
    assert_eq!(
        console_tile(&socket).await,
        TileHealth::Up { version: None },
        "a cold cache still answered"
    );

    let (_dir, socket) = socket_dir();
    serve(
        &socket,
        Peer::Reply(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ready\":true,\"services\":[{\"id\":\"trusty-console\",\"display_name\":\"Trusty Console\",\"status\":\"degraded\",\"hint\":\"overlay stalled\"}]}}\n"),
    );
    assert_eq!(
        console_tile(&socket).await,
        TileHealth::Degraded {
            reason: "overlay stalled".into()
        }
    );
    assert_eq!(ServiceId::Console.id(), CONSOLE_SERVICE);
}
