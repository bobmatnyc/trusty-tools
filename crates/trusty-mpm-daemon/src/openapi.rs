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
        crate::api::health,
        crate::api::list_sessions,
        crate::api::get_session,
        crate::api::register_session,
        crate::api::connect_session,
        crate::api::remove_session,
        crate::api::reap_sessions,
        crate::api::set_session_pid,
        crate::api::discover_sessions,
        crate::api::session_events,
        crate::api::pause_session,
        crate::api::resume_session,
        crate::api::send_command,
        crate::api::get_output,
        crate::api::recent_events,
        crate::api::ingest_hook,
        crate::api::list_projects,
        crate::api::register_project,
        crate::api::current_project,
        crate::api::discover_projects,
        crate::api::breakers,
        crate::api::get_optimizer,
        crate::api::get_overseer,
        crate::api::llm_chat,
        crate::api::list_tmux_sessions,
        crate::api::tmux_snapshot,
        crate::api::adopt_tmux_session,
        crate::api::get_claude_config,
        crate::api::apply_claude_config,
        crate::api::restart_claude_code,
        crate::api::list_checkpoints,
        crate::api::create_checkpoint,
        crate::api::restore_checkpoint,
        crate::api::delete_checkpoint,
        crate::api::list_profiles,
        crate::api::deploy_profile,
        crate::api::pair_request,
        crate::api::pair_confirm,
        crate::api::pair_status,
        crate::api::pair_reset,
        crate::api::doctor,
    ),
    components(schemas(
        trusty_mpm_core::session::Session,
        trusty_mpm_core::session::SessionStatus,
        trusty_mpm_core::session::SessionId,
        trusty_mpm_core::session::ControlModel,
        trusty_mpm_core::session::SessionHost,
        trusty_mpm_core::project::ProjectInfo,
        trusty_mpm_core::compress::CompressionLevel,
        trusty_mpm_core::external_session::ExternalSession,
        trusty_mpm_core::external_session::SessionOrigin,
        crate::optimizer::OptimizerConfig,
        crate::api::RegisterSession,
        crate::api::SetPidRequest,
        crate::api::RegisterProject,
        crate::api::HookPost,
        crate::api::PauseRequest,
        crate::api::CommandRequest,
        crate::api::AdoptRequest,
        crate::api::ApplyConfigRequest,
        crate::api::RestartRequest,
        crate::api::CreateCheckpointRequest,
        crate::api::RestoreRequest,
        crate::api::DeployProfileRequest,
        crate::api::PairConfirmRequest,
        crate::api::LlmChatRequest,
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
