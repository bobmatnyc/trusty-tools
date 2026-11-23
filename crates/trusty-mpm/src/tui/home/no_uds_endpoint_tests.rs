//! #10899: `tm tui` sends no HTTP probe. #11550: Agents left the
//! "no UDS endpoint" set when tagent gained `agents.health`; no service is in
//! it now, and the tile kind still renders as a neutral gap.

use super::analyze_probe_tests::{DEADLINE, Peer, serve, socket_dir};
use super::probe::{no_uds_endpoint, probe_service_at};
use super::render::service_lines;
use super::services::{ProbeKind, ServiceId, binary_on_path};
use super::state::{HealthUpdate, HomeState, TileHealth};

/// An `agents.health` answer from tagent 0.9.1.
const AGENTS_OK: &[u8] =
    b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"status\":\"ok\",\"version\":\"0.9.1\"}}\n";

/// Regression proof for #11550: on origin/main Agents is `NoUdsEndpoint`.
#[test]
fn agents_probe_is_uds_health_on_its_well_known_socket() {
    assert_eq!(
        ServiceId::Agents.probe(),
        ProbeKind::UdsHealth(trusty_agents_common::health::METHOD_AGENTS_HEALTH)
    );
    assert_eq!(
        ServiceId::Agents.probe(),
        ProbeKind::UdsHealth("agents.health")
    );
    assert_eq!(
        ServiceId::Agents.socket(),
        trusty_agents_common::health::agents_socket().map_err(|e| e.to_string())
    );
    assert!(ServiceId::Agents.not_listening_is_on_demand());
    // Every other tile keeps its UDS or on-demand probe.
    let others = [
        (ServiceId::Mpm, ProbeKind::UdsHealth("mpm.health")),
        (ServiceId::Search, ProbeKind::UdsHealth("search.health")),
        (ServiceId::Memory, ProbeKind::UdsHealth("memory.health")),
        (ServiceId::Analyze, ProbeKind::UdsHealth("analyze.health")),
        (ServiceId::Review, ProbeKind::OnDemand),
        (
            ServiceId::Console,
            ProbeKind::UdsFleetStatus(trusty_common::console_rpc::METHOD_FLEET_STATUS),
        ),
        (
            ServiceId::Events,
            ProbeKind::UdsHealth(trusty_common::control_bus::METHOD_EVENTS_HEALTH),
        ),
    ];
    for (id, want) in others {
        assert_eq!(id.probe(), want, "{id:?}");
    }
}

#[test]
fn no_service_maps_to_no_uds_endpoint() {
    for id in ServiceId::ALL {
        assert!(no_uds_endpoint(id).is_none(), "{id:?}");
    }
}

#[tokio::test]
async fn agents_tile_reads_up_with_version_or_on_demand() {
    let (_dir, socket) = socket_dir();
    let dialled = socket.clone();
    let idle = probe_service_at(ServiceId::Agents, move || Ok(dialled), DEADLINE).await;
    let want_idle = if binary_on_path(ServiceId::Agents.binary()) {
        TileHealth::OnDemand
    } else {
        TileHealth::Absent
    };
    assert_eq!(idle, want_idle, "nothing listening");

    serve(&socket, Peer::Reply(AGENTS_OK));
    let dialled = socket.clone();
    let up = probe_service_at(ServiceId::Agents, move || Ok(dialled), DEADLINE).await;
    if binary_on_path(ServiceId::Agents.binary()) {
        assert_eq!(
            up,
            TileHealth::Up {
                version: Some("0.9.1".into())
            }
        );
    }
}

#[test]
fn no_uds_endpoint_tiles_are_not_counted_as_down() {
    let mut state = HomeState::default();
    let health = TileHealth::NoUdsEndpoint {
        detail: "example gap",
    };
    assert!(!health.metrics_allowed(), "no console_metrics spawn");
    state.apply_health(HealthUpdate {
        id: ServiceId::Agents,
        health,
    });
    let lines: Vec<String> = service_lines(ServiceId::Agents, &state)
        .iter()
        .map(|l| l.to_string())
        .collect();
    assert!(lines.contains(&"health    no UDS endpoint".to_string()));
    assert!(lines.contains(&"detail    example gap".to_string()));
}

/// #11667: a dial-side socket lookup creates nothing. A data dir it created
/// would be 0755, which the dial's 0700 trust check then refuses.
#[test]
#[serial_test::serial]
fn dial_side_socket_lookup_leaves_no_directory_behind() {
    let (home, _guard) = crate::test_support::isolated_daemon_home(false);
    for service in [
        ServiceId::Agents,
        ServiceId::Analyze,
        ServiceId::Review,
        ServiceId::Console,
    ] {
        let socket = service.socket().expect("socket path resolves");
        assert!(socket.starts_with(home.path()), "{}", socket.display());
        let dir = home.path().join(service.id());
        assert!(
            !dir.exists(),
            "{} dial lookup created {}",
            service.id(),
            dir.display()
        );
    }
}
