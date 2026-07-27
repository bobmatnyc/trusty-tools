//! HTTP API server (#151 phase-2).
//!
//! Why: Exposes `PmResponse` over HTTP so external clients — the `ompm` thin
//! CLI, a future GUI, CI pipelines — can submit workflow tasks and poll for
//! results without spawning the orchestrator CLI themselves. Keeping the
//! server in-process with the workflow engine avoids double-spawn overhead
//! and shares the canonical response envelope.
//! What: Axum-based HTTP API + embedded web UI. The implementation is split
//! into focused submodules:
//!   - `state`        → `AppState` + task store + persistence
//!   - `auth`         → `ApiConfig`, bearer-token middleware
//!   - `routes`       → router assembly + `serve*` bootstrap
//!   - `handlers`     → task / health / docs core handlers
//!   - `cancel`       → task cancellation (`DELETE /api/task/:id`, #3063)
//!   - `models`       → inference provider catalog (`GET /api/models`, #3243)
//!   - `projects`     → project / session / agent listing handlers
//!   - `agent_patch`  → per-agent model/provider write path
//!     (`PATCH /api/agents/:name`, #3246)
//!   - `agent_knowledge` → unified knows-surface (`GET /api/agents/:name/knowledge`,
//!     #3935, DOC-57 §4): store bindings + knowledge tools + MCP connections
//!   - `project_registration` → project register + per-project config lookup
//!   - `ctrl_sessions`→ CTRL session CRUD (`om session …`)
//!   - `tm`           → tmux session management (`/api/tm/*`)
//!   - `events_sse`   → SSE telemetry stream
//!   - `listener_events` → durable eventstream-listener event list + filter
//!     toggle (`/api/listener-events*`, #3820)
//!   - `task_runner`  → subprocess workflow execution + recap dispatch
//!   - `ui`           → embedded Vite bundle serving
//!   - `workstreams`  → `GET /api/workstreams[/:name/history]` — resumable
//!     workstreams sourced from trusty-memory's `ws:<name>` tag convention
//!     (#3819, DOC-53)
//! Test: `tests` submodule + each submodule's documented coverage.

mod agent_create;
mod agent_knowledge;
mod agent_patch;
mod agent_skills;
mod agent_stores;
mod auth;
mod cancel;
mod ctrl_sessions;
mod events_sse;
mod handlers;
mod listener_events;
mod models;
mod project_registration;
mod projects;
mod relay;
mod routes;
mod state;
mod task_runner;
mod tm;
mod ui;
// `pub(crate)`, not private: `ctrl::pm_task::dispatch::classification`
// (DOC-54 §9.6) reuses this module's read/write drawer primitives
// (`list_workstream_labels_at`, `drawers_by_tag_at`,
// `create_tagged_drawer_at`) rather than duplicating the trusty-memory HTTP
// wiring — see that module's doc comment.
pub(crate) mod workstreams;

#[cfg(test)]
mod tests;

// Public API surface preserved from the pre-split `server.rs`. External
// callers (`runtime::startup`, `runtime::mode_dispatch`) use
// `serve_with_config` + `ApiConfig`; the rest are kept `pub` for tests and
// future embedders, mirroring the original module's exports.
pub use auth::ApiConfig;
// #3633 core: the `list_agents` MCP tool (`crate::runtime::mcp_serve`) reuses
// the same roster computation the `GET /api/agents` route serves, so the two
// surfaces can never drift.
#[allow(unused_imports)]
pub use handlers::TaskRequest;
pub(crate) use projects::agent_roster;
#[allow(unused_imports)]
pub use routes::{build_router, build_router_with_config, serve, serve_with_config};
#[allow(unused_imports)]
pub use state::AppState;
