//! #10899: `tm tui` sends no HTTP probe. Agents has no UDS endpoint yet, so
//! its tile reads "no UDS endpoint", never "down". #10966: Console left this
//! set when its listener (#10902) landed; see `console_fleet_status_tests`.

use std::time::Duration;

use super::console::status_of;
use super::probe::{no_uds_endpoint, probe_service};
use super::render::service_lines;
use super::services::{ProbeKind, ServiceId};
use super::state::{HealthUpdate, HomeState, TileHealth};

const NO_UDS: [ServiceId; 1] = [ServiceId::Agents];

/// Uses only the API that exists on origin/main, so it is the regression
/// proof: there the probe reads an HTTP discovery file and dials TCP.
#[tokio::test]
async fn agents_tile_reads_no_uds_endpoint() {
    for id in NO_UDS {
        let health = probe_service(id, Duration::from_millis(200)).await;
        assert_eq!(health.label(), "no UDS endpoint", "{id:?}: {health:?}");
        // #10966: the services row shows the gap in place of the tile.
        let (word, _) = status_of(&health);
        assert_eq!(word, "no UDS endpoint", "{id:?} row shows its gap");
    }
}

#[test]
fn agents_has_no_uds_endpoint_kind() {
    assert_eq!(
        ServiceId::Agents.probe(),
        ProbeKind::NoUdsEndpoint("trusty-agents has no UDS socket yet (#6433)")
    );
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

/// `no_uds_endpoint` is synchronous and takes only the id: it cannot read a
/// file, dial a socket or spawn. `probe_service` answers from it before the
/// `PATH` lookup.
#[tokio::test]
async fn a_no_uds_endpoint_probe_does_no_io_and_is_not_down() {
    for id in ServiceId::ALL {
        let pure = no_uds_endpoint(id);
        assert_eq!(pure.is_some(), NO_UDS.contains(&id), "{id:?}");
    }
    for id in NO_UDS {
        let pure = no_uds_endpoint(id).expect("no UDS endpoint");
        assert!(matches!(pure, TileHealth::NoUdsEndpoint { .. }));
        assert!(!pure.metrics_allowed(), "no console_metrics spawn");
        assert_eq!(probe_service(id, Duration::ZERO).await, pure, "{id:?}");
    }
}

#[test]
fn no_uds_endpoint_tiles_are_not_counted_as_down() {
    let mut state = HomeState::default();
    for id in NO_UDS {
        let health = no_uds_endpoint(id).expect("no UDS endpoint");
        state.apply_health(HealthUpdate { id, health });
    }
    let failing = state
        .tiles
        .iter()
        .filter(|(_, h)| {
            matches!(
                h,
                TileHealth::Down { .. } | TileHealth::Degraded { .. } | TileHealth::Timeout
            )
        })
        .count();
    assert_eq!(failing, 0);
    let lines: Vec<String> = service_lines(ServiceId::Agents, &state)
        .iter()
        .map(|l| l.to_string())
        .collect();
    assert!(lines.contains(&"health    no UDS endpoint".to_string()));
    assert!(lines.contains(&"detail    trusty-agents has no UDS socket yet (#6433)".to_string()));
    assert!(!lines.iter().any(|l| l.starts_with("socket")), "{lines:?}");
}
