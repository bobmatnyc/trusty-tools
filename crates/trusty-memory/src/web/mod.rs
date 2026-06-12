//! HTTP API + embedded SPA shell for the trusty-memory admin UI.
//!
//! Why: The web admin panel is the primary GUI for non-MCP clients. Bundling
//! the Svelte build via `rust-embed` keeps deployment to "drop the binary on
//! a host"; the JSON API surface mirrors the MCP tool set so anything
//! trusty-memory can do via Claude Code can also be done via curl or browser.
//! What: All `/api/v1/*` handlers (status, palaces, drawers, recall, KG,
//! config, chat) plus an embedded-asset fallback that serves `ui/dist/`.
//! Test: `cargo test -p trusty-memory web::tests` covers the asset
//! fallback and JSON shape of every read endpoint against an in-memory
//! palace built on a `tempdir`.

use axum::{
    routing::{delete, get, post},
    Router,
};

use crate::AppState;

pub(crate) mod activity;
pub(crate) mod admin;
pub(crate) mod error;
pub(crate) mod health;
pub(crate) mod kg_routes;
pub(crate) mod palace_routes;
pub(crate) mod prompt_context;
pub(crate) mod recall_routes;
pub(crate) mod rpc;
pub(crate) mod static_assets;

// Re-export the pub(crate) items that other modules (primarily `chat.rs`)
// reference via `crate::web::`. Only items that are actually imported from
// outside the `web/` module are listed here; submodule-internal items are
// accessed directly from their source module within the `web/` hierarchy.
pub(crate) use error::{open_handle, ApiError};
pub(crate) use kg_routes::DreamStatusPayload;
pub(crate) use palace_routes::{load_user_config, palace_info_from};
pub(crate) use rpc::creator_info_from_http;

/// Dedicated palace id used by the `/health` round-trip probe (issue #185).
///
/// Why: Earlier revisions of `run_health_round_trip` picked whichever palace
/// happened to be first on disk (APFS creation order on macOS), which meant
/// the probe always wrote — and, if recall failed, *leaked* — a drawer in a
/// real user-facing palace. Routing the probe to a dedicated palace whose id
/// starts with the reserved `__` prefix means leaked drawers are confined to a
/// palace the user never sees (filtered by `MemoryService::list_palaces`) and
/// real palaces stay clean.
/// What: A constant `&str` reused by the probe and tests. The leading double
/// underscore is the project-wide convention for "system" palaces hidden from
/// user listings.
/// Test: `health_probe_palace_is_invisible`, `health_probe_cleans_up_on_success`,
/// `health_probe_cleans_up_on_recall_miss`.
pub(crate) const HEALTH_PROBE_PALACE: &str = "__health_probe__";

/// Build the public router with API routes + SPA asset fallback.
///
/// Why: `run_http` calls this so the same router shape is used in tests.
/// What: All API routes under `/api/v1`, fallback to the SPA shell.
/// Test: `serves_index_html_fallback` and `status_endpoint_returns_payload`.
pub fn router() -> Router<AppState> {
    // axum 0.8 path syntax uses `{param}` instead of `:param`. The shared
    // `trusty_common::server::with_standard_middleware` layer brings in CORS,
    // tracing, and gzip (with SSE excluded) so we don't drift from sibling
    // trusty-* daemons.
    let router = Router::new()
        .route("/api/v1/status", get(palace_routes::status))
        .route("/api/v1/config", get(palace_routes::config))
        .route(
            "/api/v1/palaces",
            get(palace_routes::list_palaces).post(palace_routes::create_palace),
        )
        .route(
            "/api/v1/palaces/{id}",
            get(palace_routes::get_palace_handler)
                .delete(palace_routes::delete_palace_handler)
                .patch(palace_routes::update_palace_handler),
        )
        .route(
            "/api/v1/palaces/{id}/drawers",
            get(palace_routes::list_drawers).post(palace_routes::create_drawer),
        )
        .route(
            "/api/v1/palaces/{id}/drawers/{drawer_id}",
            delete(palace_routes::delete_drawer),
        )
        // Issue #70 — `/memories` is a backward-compatible alias for `/drawers`.
        // Some clients (and earlier docs) POST/GET against `…/memories`, which
        // 404'd because only `/drawers` was registered. Aliasing here keeps
        // both vocabularies working against the same handlers without breaking
        // existing `/drawers` callers.
        .route(
            "/api/v1/palaces/{id}/memories",
            get(palace_routes::list_drawers).post(palace_routes::create_drawer),
        )
        .route(
            "/api/v1/palaces/{id}/memories/{drawer_id}",
            delete(palace_routes::delete_drawer),
        )
        .route(
            "/api/v1/palaces/{id}/recall",
            get(recall_routes::recall_handler),
        )
        .route("/api/v1/recall", get(recall_routes::recall_all_handler))
        .route(
            "/api/v1/palaces/{id}/kg",
            get(kg_routes::kg_query).post(kg_routes::kg_assert),
        )
        .route(
            "/api/v1/palaces/{id}/kg/subjects",
            get(kg_routes::kg_list_subjects),
        )
        .route(
            "/api/v1/palaces/{id}/kg/subjects_with_counts",
            get(kg_routes::kg_list_subjects_with_counts),
        )
        .route("/api/v1/palaces/{id}/kg/all", get(kg_routes::kg_list_all))
        .route("/api/v1/palaces/{id}/kg/graph", get(kg_routes::kg_graph))
        .route("/api/v1/palaces/{id}/kg/count", get(kg_routes::kg_count))
        .route(
            "/api/v1/palaces/{id}/kg/triples/{triple_id}",
            delete(kg_routes::kg_delete_triple),
        )
        .route(
            "/api/v1/palaces/{id}/dream/status",
            get(kg_routes::palace_dream_status),
        )
        .route("/api/v1/dream/status", get(kg_routes::dream_status))
        .route("/api/v1/dream/run", post(kg_routes::dream_run))
        .route("/api/v1/kg/gaps", get(prompt_context::kg_gaps_handler))
        .route(
            "/api/v1/kg/prompt-context",
            get(prompt_context::prompt_context_handler),
        )
        .route(
            "/api/v1/kg/aliases",
            post(prompt_context::add_alias_handler),
        )
        .route(
            "/api/v1/kg/prompt-facts",
            get(prompt_context::list_prompt_facts_handler)
                .delete(prompt_context::remove_prompt_fact_handler),
        )
        .route("/api/v1/chat", post(crate::chat::chat_handler))
        .route("/api/v1/chat/providers", get(crate::chat::list_providers))
        .route(
            "/api/v1/palaces/{id}/chat/sessions",
            get(crate::chat::list_chat_sessions).post(crate::chat::create_chat_session),
        )
        .route(
            "/api/v1/palaces/{id}/chat/sessions/{session_id}",
            get(crate::chat::get_chat_session).delete(crate::chat::delete_chat_session),
        )
        // Issue #99: inter-project messaging.
        .route(
            "/api/v1/messages",
            get(crate::chat::list_messages_handler).post(crate::chat::send_message_handler),
        )
        .route(
            "/api/v1/messages/mark_read",
            post(crate::chat::mark_message_read_handler),
        )
        .route("/health", get(health::health))
        .route("/api/v1/logs/tail", get(admin::logs_tail))
        .route("/api/v1/activity", get(activity::activity_handler))
        .route(
            "/api/v1/activity/hook",
            post(activity::hook_activity_handler),
        )
        .route("/api/v1/admin/stop", post(admin::admin_stop))
        // Issue: fire-and-forget memory save for callers that cannot speak
        // MCP. Sub-agents spawned via Claude Code's Agent tool inherit no
        // MCP connections, so `memory_remember` is unreachable to them.
        // This endpoint lets the agent shell out to `trusty-memory note`
        // (which in turn POSTs here) and the request returns 202 the moment
        // the body is parsed — the actual `memory_remember` dispatch runs
        // on a detached `tokio::spawn`. Failures are logged at warn but
        // never surface to the caller because the contract is one-way.
        .route("/api/v1/remember", post(admin::remember_async))
        // Multi-transport refactor: a single JSON-RPC 2.0 endpoint that
        // accepts the same envelopes the UDS transport speaks. Lets
        // browser clients, curl, and the stdio bridge fallback hit the
        // tool surface without learning the REST routes. The REST
        // routes above remain for backwards compatibility.
        .route("/rpc", post(rpc::rpc_handler))
        .fallback(static_assets::static_handler);

    trusty_common::server::with_standard_middleware(router)
}

#[cfg(test)]
mod tests;
