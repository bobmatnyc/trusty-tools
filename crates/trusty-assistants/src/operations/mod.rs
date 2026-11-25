//! Assistant channel, listener and knowledge operations, transport-free.
//!
//! Why (#11632): these modules lived under `api::server` because the HTTP API
//! was their first caller, but the kept assistant core — the `channel` tool,
//! the channel adapters, `knowledge_history`, the memory grants — calls them
//! too. The HTTP routes were cut with the API; the operations stay here so
//! the kept callers behave as before.
//! What: per-assistant channel bindings and sends (`agent_channels`), the
//! listener table (`agent_listeners`), the global channel table
//! (`global_channels`), the channel-write credential gate (`channel_auth`),
//! the knowledge pipeline's intake and history (`knowledge_pipeline`), and two
//! path helpers (`agent_paths`, `agent_names`).
//! Test: each submodule's own tests.

pub(crate) mod agent_channels;
pub(crate) mod agent_listeners;
pub(crate) mod agent_names;
pub(crate) mod agent_paths;
pub(crate) mod channel_auth;
pub(crate) mod global_channels;
// #11550: agents.health on the well-known socket `tm tui` probes; moved here
// from the cut `api::server` (#11632).
pub(crate) mod health_socket;
pub(crate) mod knowledge_pipeline;

/// The status half of an operation error. #11632: was `axum::http::StatusCode`;
/// `reqwest` re-exports the same `http` type, so no dependency is added.
pub(crate) use reqwest::StatusCode;

/// The JSON body half of an operation error, `(StatusCode, Json(value))`.
///
/// Why (#11632): the operations kept the error shape the HTTP routes used, so
/// their callers and tests did not change; `axum` itself left with the routes.
/// What: a plain newtype over the body.
/// Test: the operations' own error-path tests destructure it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Json<T>(pub T);

/// The assistant-name argument the knowledge operations take. #11632: was
/// `axum::extract::Path`.
pub(crate) struct AxumPath<T>(pub T);

/// Serialize all assistant manifest read-modify-write operations in this process.
static AGENT_CONFIG_WRITE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
