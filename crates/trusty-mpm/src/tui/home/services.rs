//! The service roster the console home screen draws one tile per entry for.
//!
//! Why (#10491): `tm tui` opens on the same roster trusty-console shows
//! (`trusty-console/src/detect/mod.rs::all_connectors`). trusty-mpm must not
//! depend on trusty-console (#1318), so the roster's facts — binary names,
//! health methods, socket paths and `console_metrics` launch commands — are
//! ported here as data, not imported.
//! What: [`ServiceId`] and the per-service [`ProbeKind`] and socket resolver.
//! Test: `roster_lists_mpm_first_then_the_console_order`,
//! `home_opens_with_mpm_first_and_selected`.

use std::path::PathBuf;

/// One trusty-* service the console knows, in the console's display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceId {
    /// trusty-search, `search.health` on its socket.
    Search,
    /// trusty-memory, `memory.health` on its socket.
    Memory,
    /// trusty-analyze, `analyze.health` on its socket.
    Analyze,
    /// trusty-review, an on-demand stdio service with no resident daemon.
    Review,
    /// trusty-mpm, `mpm.health` on its socket. Enter opens the projects view.
    Mpm,
    /// trusty-agents; no UDS socket yet, so the TUI does not probe it (#10899).
    Agents,
    /// trusty-events, the bus daemon, `events.health` on its socket.
    Events,
    /// trusty-console; no UDS listener yet, so the TUI does not probe it (#10899).
    Console,
}

/// How a tile's health is observed. Every kind uses an existing interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeKind {
    /// One JSON-RPC `<method>` frame over the service's Unix socket.
    UdsHealth(&'static str),
    /// One connect to the service's Unix socket, closed at once.
    SocketConnect,
    /// The service serves no Unix socket the TUI may dial, so it is not probed.
    ///
    /// Why (#10899): `tm tui` uses no native HTTP; services are reached over
    /// UDS only (ADR-0032). A service with no UDS endpoint offers the TUI
    /// nothing to observe, and that is not a failure.
    /// What: carries the operator-facing detail naming the server-side gap.
    /// The probe maps it straight to `TileHealth::NoUdsEndpoint` with no file
    /// read, socket or process spawn.
    /// Test: `agents_and_console_have_no_uds_endpoint_kind`,
    /// `a_no_uds_endpoint_probe_does_no_io_and_is_not_down`.
    NoUdsEndpoint(&'static str),
    /// No resident daemon: the binary on `PATH` is the whole answer.
    OnDemand,
}

impl ServiceId {
    /// Every service: MPM first, then the console's `all_connectors` order.
    // #10476: MPM tile first and selected on launch
    pub const ALL: [ServiceId; 8] = [
        ServiceId::Mpm,
        ServiceId::Search,
        ServiceId::Memory,
        ServiceId::Analyze,
        ServiceId::Review,
        ServiceId::Agents,
        ServiceId::Events,
        ServiceId::Console,
    ];

    /// The machine id the console uses for this service.
    pub fn id(self) -> &'static str {
        match self {
            ServiceId::Search => "trusty-search",
            ServiceId::Memory => "trusty-memory",
            ServiceId::Analyze => "trusty-analyze",
            ServiceId::Review => "trusty-review",
            ServiceId::Mpm => "trusty-mpm",
            ServiceId::Agents => "trusty-agents",
            ServiceId::Events => "trusty-events",
            ServiceId::Console => "trusty-console",
        }
    }

    /// The tile title.
    pub fn display_name(self) -> &'static str {
        match self {
            ServiceId::Search => "Search",
            ServiceId::Memory => "Memory",
            ServiceId::Analyze => "Analyze",
            ServiceId::Review => "Review",
            ServiceId::Mpm => "MPM",
            ServiceId::Agents => "Agents",
            ServiceId::Events => "Events",
            ServiceId::Console => "Console",
        }
    }

    /// The binary whose presence on `PATH` separates "stopped" from "absent".
    pub fn binary(self) -> &'static str {
        match self {
            ServiceId::Mpm => "tm",
            ServiceId::Agents => "tagent",
            other => other.id(),
        }
    }

    /// How this service's health is observed (ported from the console's
    /// `detect/*.rs` connectors).
    pub fn probe(self) -> ProbeKind {
        match self {
            ServiceId::Search => ProbeKind::UdsHealth("search.health"),
            ServiceId::Memory => ProbeKind::UdsHealth("memory.health"),
            ServiceId::Analyze => ProbeKind::UdsHealth("analyze.health"),
            ServiceId::Mpm => ProbeKind::UdsHealth("mpm.health"),
            // See #10901: the bus answers health, not just a connect.
            ServiceId::Events => {
                ProbeKind::UdsHealth(trusty_common::control_bus::METHOD_EVENTS_HEALTH)
            }
            // See #10899: the TUI sends no HTTP probe; these have no UDS yet.
            ServiceId::Agents => {
                ProbeKind::NoUdsEndpoint("trusty-agents has no UDS socket yet (#6433)")
            }
            ServiceId::Console => {
                ProbeKind::NoUdsEndpoint("trusty-console has no UDS listener yet (ADR-0035)")
            }
            ServiceId::Review => ProbeKind::OnDemand,
        }
    }

    /// Whether a probe that found nothing listening means "idle on demand",
    /// not "down". trusty-console's analyze connector makes the same call
    /// (#6350, #6416); any other probe failure is still down.
    pub fn not_listening_is_on_demand(self) -> bool {
        // #10476: analyze is on demand; not listening is idle, not down
        matches!(self, ServiceId::Analyze)
    }

    /// The stdio MCP command trusty-console spawns to call `console_metrics`
    /// (`trusty-console/src/server/mod.rs`), or `None` when the service
    /// offers no such tool. trusty-mpm returns `None` because its Enter opens
    /// the projects view instead.
    pub fn metrics_command(self) -> Option<(&'static str, &'static [&'static str])> {
        match self {
            ServiceId::Search => Some(("trusty-search", &["serve"])),
            ServiceId::Memory => Some(("trusty-memory", &["serve", "--stdio"])),
            ServiceId::Analyze => Some(("trusty-analyze", &["mcp"])),
            ServiceId::Review => Some(("trusty-review", &["serve", "--stdio"])),
            ServiceId::Mpm | ServiceId::Agents | ServiceId::Events | ServiceId::Console => None,
        }
    }

    /// The Unix socket a socket probe dials, resolved the way the service and
    /// the console resolve it. `Err` carries an operator-facing reason.
    pub fn socket(self) -> Result<PathBuf, String> {
        match self {
            ServiceId::Search => trusty_common::search_rpc::search_socket()
                .map_err(|e| format!("search socket unresolved: {e:#}")),
            ServiceId::Memory => trusty_common::memory_rpc::resolve_memory_socket()
                .map_err(|e| format!("memory socket unresolved: {e:#}")),
            ServiceId::Mpm => Ok(trusty_common::mpm_rpc::mpm_socket()),
            ServiceId::Events => trusty_common::control_bus::events_socket_path()
                .map_err(|e| format!("events socket unresolved: {e:#}")),
            other => trusty_common::daemon_socket_path(other.id())
                .map_err(|e| format!("{} socket unresolved: {e:#}", other.id())),
        }
    }
}

/// Whether `binary` resolves on `PATH`.
pub fn binary_on_path(binary: &str) -> bool {
    which::which(binary).is_ok()
}
