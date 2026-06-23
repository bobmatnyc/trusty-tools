//! trusty-mpm shared client crate.
//!
//! Why: the Telegram bot, the TUI dashboard, and the CLI all spoke to the
//! daemon through three independent HTTP layers — the bot inlined `reqwest`
//! calls, the TUI carried its own `DaemonClient`, and the CLI built requests by
//! hand. That triplication meant every new endpoint had to be wired three times
//! and the three UIs drifted apart. This crate is the single shared seam:
//! [`DaemonClient`] is the one HTTP transport, [`TrustyCommand`] the one command
//! model, [`CommandResult`] the one UI-agnostic result type, and
//! [`CommandExecutor`] the only place that translates a command into daemon
//! HTTP calls.
//! What: re-exports the four building blocks so a UI crate depends on exactly
//! one shared layer.
//! Test: `cargo test -p trusty-mpm-client` covers URL construction, the command
//! model, and the executor against an in-process test daemon.

pub mod catalog;
pub mod command;
pub mod executor;
pub mod http_client;
pub mod resolver;
pub mod result;

pub use crate::core::doctor::{CheckStatus, DoctorCheck, DoctorReport};
pub use catalog::{CommandSpec, SpecKind, catalog, render_sm_tools};
pub use command::TrustyCommand;
pub use executor::CommandExecutor;
pub use http_client::{
    BreakerRow, ChatMessage, ConfigRecommendation, CoordinatorChatOutcome, CoordinatorContext,
    CoordinatorSession, DaemonClient, DiscoveredProjectRow, EventRow, FleetByProjectWireResponse,
    FleetProjectGroupWire, HealthSnapshot, LastSeen, LlmChatOutcome, ManagedActivityResponse,
    ManagedAdoptRequest, ManagedAdoptResponse, ManagedAnswerRequest, ManagedAnswerResponse,
    ManagedAttachCmdResponse, ManagedListResponse, ManagedSendInputRequest,
    ManagedSendInputResponse, ManagedSessionSummary, ManagedSpawnRequest, ManagedSpawnResponse,
    OverseerSnapshot, PairConfirm, PairRequest, PairStatus, SessionRow, TmuxSessionRow,
};
pub use resolver::{Resolvable, resolve_target};
pub use result::{
    CommandResult, DecisionCounts, DiscoveredProjectSummary, HealthReport, ManagedSessionView,
    ProjectFleetView, RecommendationSummary, SessionSummary, TmuxSessionSummary,
};
