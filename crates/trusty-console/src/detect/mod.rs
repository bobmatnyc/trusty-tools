//! Concrete `ServiceConnector` implementations for the trusty-* services.
//!
//! Why: the console's Overview reports each service read-only — installed,
//! running, which version — and falls back gracefully when the daemon or
//! binary is absent.
//! What: one connector per service. Each checks for its binary on PATH
//! (`Absent` when missing), then asks the daemon on the transport it serves:
//! trusty-search, trusty-memory and (since #6288) trusty-mpm answer a health
//! method on their Unix socket; trusty-agents still publishes an `http_addr`
//! file the connector TCP-probes; trusty-analyze and trusty-review are
//! on-demand members.
//! Test: unit tests live in each submodule and point a connector at a socket
//! or data directory the test owns, never the operator's. Run with
//! `cargo test -p trusty-console`.

mod agents;
mod analyze;
mod console;
mod helpers;
mod memory;
mod mpm;
mod review;
mod search;
// #6288: the health dial the memory and mpm connectors share.
mod socket_health;

pub use agents::AgentsConnector;
pub use analyze::AnalyzeConnector;
pub use console::ConsoleConnector;
pub use memory::MemoryConnector;
pub use mpm::MpmConnector;
pub use review::ReviewConnector;
pub use search::SearchConnector;

use crate::connector::{ServiceConnector, ServiceInfo, ServiceStatus};

/// Process-wide lock serialising every test that mutates the global
/// `TRUSTY_DATA_DIR_OVERRIDE` env var.
///
/// Why: the agents connector tests (and, before #6288, the mpm ones) point `resolve_data_dir` at a
/// tempdir via that ONE process-global env var. Per-module locks do NOT
/// serialise across modules, so two such tests in different modules could
/// clobber each other's override and race (#3331 regression: the agents tests
/// racing the mpm lock-file test). A single shared lock in the common parent
/// module serialises them all.
///
/// #6661: `lib.rs` kept a SECOND lock over the same variable, so its
/// `remove_var` landed inside this lock's critical section and
/// an mpm connector test read the live daemon on 7880
/// instead of its fixture. This is now the crate's ONE lock for
/// `TRUSTY_DATA_DIR_OVERRIDE`; a new env-mutating test takes this one or it is
/// not serialised at all. (Under `cargo nextest` each test owns its process,
/// so the variable is not shared there and the lock is inert — see CLAUDE.md.)
/// What: a `std::sync::Mutex<()>` locked by every env-mutating test in the crate.
/// Test: used by `agents::tests`, `search_uds::tests`, the `service_metrics`
/// mpm pid test (#6288, which also sets `TRUSTY_MPM_SOCKET` under it) and the
/// `port` verb tests in `lib.rs`; not itself a test.
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Return all connectors in display order.
///
/// Why: Centralises the connector list so the server and any future CLI
/// command iterate the same set. Issue #1163 lifts the prior #1069 exclusion
/// of trusty-review: the Review dashboard tab is now fully implemented with a
/// `console_metrics` MCP tool, so the service is included in the Overview.
/// What: Returns a `Vec<Box<dyn ServiceConnector>>` with seven connectors:
/// search, memory, analyze, review, mpm (#1222 adds trusty-mpm for the Sessions
/// tab), agents (#3331 adds trusty-agents so `/api/agents/*` resolves an
/// upstream under the loopback-only doctrine), console (#6908 adds the console
/// itself, which was the one local service missing from its own roster).
/// Test: `test_all_connectors_returns_seven` below.
pub fn all_connectors() -> Vec<Box<dyn ServiceConnector>> {
    vec![
        Box::new(SearchConnector::new()),
        Box::new(MemoryConnector::new()),
        Box::new(AnalyzeConnector::new()),
        Box::new(ReviewConnector::new()),
        Box::new(MpmConnector::new()),
        Box::new(AgentsConnector::new()),
        // #6908: the console lists itself, so the roster covers every local
        // service rather than every local service except the one serving it.
        Box::new(ConsoleConnector::new()),
    ]
}

/// Liveness rank used to sort the Overview grid — lower sorts first.
///
/// Why: #6370 — the dashboard showed connector-registration order, so a service
/// that is not installed could sit above the one the operator is running.
/// What: Running 0, Degraded 1, Available 2, Absent 3 — the order in which a
/// service has something to show: a live daemon, a reachable-but-impaired
/// daemon, an installed binary, then nothing.
/// Test: `order_for_display_puts_running_first`.
fn liveness_rank(info: &ServiceInfo) -> u8 {
    match info.status {
        ServiceStatus::Running => 0,
        ServiceStatus::Degraded => 1,
        ServiceStatus::Available => 2,
        ServiceStatus::Absent => 3,
    }
}

/// Sort detected services into dashboard display order.
///
/// Why: #6370 — largest / most recently active service first. `ServiceInfo`
/// carries no size or activity counter, so liveness is the only activity signal
/// the payload has; when a size metric lands it becomes the secondary key
/// inside a rank.
/// What: A STABLE sort on `liveness_rank`, so within one rank the
/// `all_connectors()` registration order survives as the tiebreak and the grid
/// never reshuffles between two polls that detected the same statuses.
/// Test: `order_for_display_puts_running_first`,
/// `order_for_display_is_stable_within_a_rank`,
/// `order_for_display_is_idempotent`.
pub fn order_for_display(services: &mut [ServiceInfo]) {
    services.sort_by_key(liveness_rank);
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::ServiceLifecycle;

    /// Why: the registry must return exactly seven connectors in order (search,
    /// memory, analyze, review, mpm — #1222; agents — #3331 for the
    /// `/api/agents/*` proxy under the loopback-only doctrine; console — #6908,
    /// the console's own row).
    /// What: calls all_connectors() and checks IDs.
    /// Test: this test itself.
    #[test]
    fn test_all_connectors_returns_eight() {
        let cs = all_connectors();
        assert_eq!(cs.len(), 8);
        assert_eq!(cs[0].id(), "trusty-search");
        assert_eq!(cs[1].id(), "trusty-memory");
        assert_eq!(cs[2].id(), "trusty-analyze");
        assert_eq!(cs[3].id(), "trusty-review");
        assert_eq!(cs[4].id(), "trusty-mpm");
        assert_eq!(cs[5].id(), "trusty-agents");
        // #10042: the event bus daemon's socket state.
        assert_eq!(cs[6].id(), "trusty-events");
        assert_eq!(cs[7].id(), "trusty-console");
    }

    /// REGRESSION (#6416): the de-daemonized members must be exactly
    /// trusty-review (#6290 retired its daemon) and trusty-analyze (#6287 and
    /// #6350 made its socket server on-demand). Every other member is a resident
    /// daemon whose `Available` really does mean "installed but stopped".
    ///
    /// Why: this reads the per-member constant off the trait rather than running
    /// a detection pass, so it asserts the roster without dialling a socket or
    /// spawning a binary.
    /// What: partitions `all_connectors()` by lifecycle and names both halves.
    /// Test: this test itself.
    #[test]
    fn on_demand_members_are_exactly_review_and_analyze() {
        let connectors = all_connectors();
        let on_demand: Vec<&str> = connectors
            .iter()
            .filter(|c| c.lifecycle() == ServiceLifecycle::OnDemand)
            .map(|c| c.id())
            .collect();
        assert_eq!(on_demand, vec!["trusty-analyze", "trusty-review"]);

        let daemons: Vec<&str> = connectors
            .iter()
            .filter(|c| c.lifecycle() == ServiceLifecycle::Daemon)
            .map(|c| c.id())
            .collect();
        // #6908: trusty-console joins the daemon half — it is a resident
        // process, and its `Running` is the only status it can report.
        assert_eq!(
            daemons,
            vec![
                "trusty-search",
                "trusty-memory",
                "trusty-mpm",
                "trusty-agents",
                "trusty-events",
                "trusty-console"
            ]
        );
    }

    /// Build a `ServiceInfo` carrying only the two fields ordering reads.
    fn info(id: &str, status: ServiceStatus) -> ServiceInfo {
        ServiceInfo {
            id: id.to_string(),
            display_name: id.to_string(),
            status,
            version: None,
            url: None,
            hint: None,
            lifecycle: ServiceLifecycle::Daemon,
            cpu_pct: None,
            rss_bytes: None,
        }
    }

    fn ids(services: &[ServiceInfo]) -> Vec<&str> {
        services.iter().map(|s| s.id.as_str()).collect()
    }

    /// Why: #6370 — a card for a service that is not installed must not sit
    /// above the daemon the operator is running.
    /// What: feeds the four statuses in reverse rank order and asserts the sort
    /// inverts them.
    /// Test: this test itself.
    #[test]
    fn order_for_display_puts_running_first() {
        let mut services = vec![
            info("absent-one", ServiceStatus::Absent),
            info("available-one", ServiceStatus::Available),
            info("degraded-one", ServiceStatus::Degraded),
            info("running-one", ServiceStatus::Running),
        ];
        order_for_display(&mut services);
        assert_eq!(
            ids(&services),
            vec!["running-one", "degraded-one", "available-one", "absent-one"]
        );
    }

    /// Why: two services with the same status must keep registration order, or
    /// the grid reshuffles between polls that detected nothing new.
    /// What: three Running services in registration order stay in that order.
    /// Test: this test itself.
    #[test]
    fn order_for_display_is_stable_within_a_rank() {
        let mut services = vec![
            info("trusty-search", ServiceStatus::Running),
            info("trusty-memory", ServiceStatus::Running),
            info("trusty-analyze", ServiceStatus::Running),
        ];
        order_for_display(&mut services);
        assert_eq!(
            ids(&services),
            vec!["trusty-search", "trusty-memory", "trusty-analyze"]
        );
    }

    /// Why: the route sorts every response, so sorting an already-sorted list
    /// must not move anything.
    /// What: sorts twice and compares the two orders.
    /// Test: this test itself.
    #[test]
    fn order_for_display_is_idempotent() {
        let mut services = vec![
            info("b", ServiceStatus::Absent),
            info("a", ServiceStatus::Running),
            info("c", ServiceStatus::Absent),
        ];
        order_for_display(&mut services);
        let once: Vec<String> = services.iter().map(|s| s.id.clone()).collect();
        order_for_display(&mut services);
        let twice: Vec<String> = services.iter().map(|s| s.id.clone()).collect();
        assert_eq!(once, twice);
    }
}
