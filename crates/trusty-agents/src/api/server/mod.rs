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
//!   - `project_registration` → project register + per-project config lookup
//!   - `ctrl_sessions`→ CTRL session CRUD (`om session …`)
//!   - `tm`           → tmux session management (`/api/tm/*`)
//!   - `events_sse`   → SSE telemetry stream
//!   - `task_runner`  → subprocess workflow execution + recap dispatch
//!   - `ui`           → embedded Vite bundle serving
//! Test: `tests` submodule + each submodule's documented coverage.

mod agent_patch;
mod auth;
mod cancel;
mod ctrl_sessions;
mod events_sse;
mod handlers;
mod models;
mod project_registration;
mod projects;
mod relay;
mod routes;
mod state;
mod task_runner;
mod tm;
mod ui;

#[cfg(test)]
mod tests;

// Public API surface preserved from the pre-split `server.rs`. External
// callers (`runtime::startup`, `runtime::mode_dispatch`) use
// `serve_with_config` + `ApiConfig`; the rest are kept `pub` for tests and
// future embedders, mirroring the original module's exports.
pub use auth::ApiConfig;
#[allow(unused_imports)]
pub use handlers::TaskRequest;
#[allow(unused_imports)]
pub use routes::{build_router, build_router_with_config, serve, serve_with_config};
#[allow(unused_imports)]
pub use state::AppState;
