//! `search.file.get` — resolve the index, then read one file off the runtime
//! (#9029).
//!
//! Why: index resolution (including a cold-parked reload) belongs to
//! [`super::index_resolve::resolve_or_load_index`], which only this module tree
//! can reach; the file read itself is [`crate::service::file_view`].
//! What: [`file_get_report`] and its params.
//! Test: `rpc/file_tests.rs`.

use std::sync::Arc;

use axum::http::StatusCode;
use serde::Deserialize;

use crate::core::registry::IndexId;
use crate::service::file_view::{read_indexed_file, DiffMode, Limits};

use super::state::SearchAppState;

/// `search.file.get` params.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileGetParams {
    /// The index whose root the path lives under.
    pub index_id: String,
    /// Root-relative or absolute path of the file.
    pub path: String,
    /// `none` (default) or `head`.
    #[serde(default)]
    pub diff: DiffMode,
}

/// Serve one indexed file, and its diff when asked.
///
/// Why: the dashboard's file viewer (#9029) needs the whole file, not a chunk.
/// What: resolves the index exactly as search does, loading a cold-parked one,
/// then runs [`read_indexed_file`] on the blocking pool with the default
/// [`Limits`]. Refusals keep their `(status, body)` shape.
/// Test: `file_get_over_the_daemon_socket_returns_content_and_head_diff`,
/// `file_get_refusals_keep_their_codes_over_the_socket`.
pub(crate) async fn file_get_report(
    state: &Arc<SearchAppState>,
    params: FileGetParams,
) -> Result<serde_json::Value, (StatusCode, serde_json::Value)> {
    let index_id = IndexId::new(params.index_id);
    let handle = super::index_resolve::resolve_or_load_index(state, &index_id)
        .await
        .map_err(|(status, body)| (status, body.0))?;
    let FileGetParams { path, diff, .. } = params;
    tokio::task::spawn_blocking(move || read_indexed_file(&handle, &path, diff, &Limits::default()))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                serde_json::json!({
                    "error": "file_read_failed",
                    "index_id": index_id.0,
                    "message": format!("the read task failed: {e}"),
                }),
            )
        })?
}
