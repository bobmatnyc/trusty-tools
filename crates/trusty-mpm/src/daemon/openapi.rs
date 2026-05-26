//! OpenAPI 3.1 specification for the daemon HTTP API.
//!
//! Why: the daemon's HTTP surface is consumed by the CLI, TUI, Telegram bot,
//! and ad-hoc `curl` debugging; a machine-readable spec plus a Swagger UI makes
//! the contract discoverable and lets clients be generated rather than
//! hand-written.
//! What: the [`ApiDoc`] type derives a [`utoipa::OpenApi`] document from the
//! `#[utoipa::path]`-annotated handlers in [`crate::api`] and the
//! `#[derive(ToSchema)]` types they exchange.
//! Test: `openapi_spec_is_valid` in `api.rs` asserts `GET
//! /api-docs/openapi.json` returns a document with an `openapi` key and the
//! correct title.

use utoipa::OpenApi;

/// The daemon's complete OpenAPI 3.1 document.
///
/// Why: one struct aggregates every annotated path and schema so the router
/// can serve a single, consistent spec.
/// What: lists each handler function and each `ToSchema` type, plus the API
/// tags used to group endpoints in Swagger UI.
/// Test: `openapi_spec_is_valid`.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "trusty-mpm daemon API",
        version = "0.1.0",
        description = "HTTP API for the trusty-mpm Claude Code session manager daemon."
    ),
    paths(
        super::api::health,
        super::api::list_sessions,
        super::api::get_session,
        super::api::register_session,
        super::api::connect_session,
        super::api::remove_session,
        super::api::reap_sessions,
        super::api::set_session_pid,
        super::api::discover_sessions,
        super::api::session_events,
        super::api::pause_session,
        super::api::resume_session,
        super::api::send_command,
        super::api::get_output,
        super::api::recent_events,
        super::api::ingest_hook,
        super::api::list_projects,
        super::api::register_project,
        super::api::current_project,
        super::api::discover_projects,
        super::api::breakers,
        super::api::get_optimizer,
        super::api::get_overseer,
        super::api::llm_chat,
        super::api::list_tmux_sessions,
        super::api::tmux_snapshot,
        super::api::adopt_tmux_session,
        super::api::get_claude_config,
        super::api::apply_claude_config,
        super::api::restart_claude_code,
        super::api::list_checkpoints,
        super::api::create_checkpoint,
        super::api::restore_checkpoint,
        super::api::delete_checkpoint,
        super::api::list_profiles,
        super::api::deploy_profile,
        super::api::pair_request,
        super::api::pair_confirm,
        super::api::pair_status,
        super::api::pair_reset,
        super::api::doctor,
    ),
    components(schemas(
        crate::core::session::Session,
        crate::core::session::SessionStatus,
        crate::core::session::SessionId,
        crate::core::session::ControlModel,
        crate::core::session::SessionHost,
        crate::core::project::ProjectInfo,
        crate::core::compress::CompressionLevel,
        crate::core::external_session::ExternalSession,
        crate::core::external_session::SessionOrigin,
        super::optimizer::OptimizerConfig,
        super::api::RegisterSession,
        super::api::SetPidRequest,
        super::api::RegisterProject,
        super::api::HookPost,
        super::api::PauseRequest,
        super::api::CommandRequest,
        super::api::AdoptRequest,
        super::api::ApplyConfigRequest,
        super::api::RestartRequest,
        super::api::CreateCheckpointRequest,
        super::api::RestoreRequest,
        super::api::DeployProfileRequest,
        super::api::PairConfirmRequest,
        super::api::LlmChatRequest,
    )),
    tags(
        (name = "sessions", description = "Session lifecycle management"),
        (name = "projects", description = "Project registry"),
        (name = "events", description = "Hook event feed"),
        (name = "config", description = "Runtime configuration"),
        (name = "tmux", description = "Universal tmux session management"),
        (name = "claude-config", description = "Claude Code configuration analyzer"),
        (name = "internal", description = "Internal machine-to-machine endpoints"),
    )
)]
pub struct ApiDoc;
