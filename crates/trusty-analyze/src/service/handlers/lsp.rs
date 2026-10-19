//! `analyze.lsp_diagnostics`, `analyze.lsp_references` and
//! `analyze.lsp_analysed_at` (#9078, L3).
//!
//! Why: DOC-72 §5 serves type-checked answers from the on-demand language
//! server on the analyze socket. §2 forbids failing open and §3 requires every
//! answer to name the document version and workspace snapshot it describes.
//! What: each handler checks the default-off flag, resolves the file's
//! workspace root, leases that root's server (spawning it if needed), syncs the
//! file from disk, stamps the workspace snapshot, refuses a stale `version`,
//! waits for the server to report itself quiescent (a loading server answers
//! `[]`, which must never become a success), asks the server, and stamps the
//! answer with `document_version` and `snapshot`. Every failure is a typed
//! refusal (`lsp_rpc_error`); none becomes an empty success. Registered with the
//! ordinary `typed` registration, so a call re-arms the idle window.
//! Test: `tests/lsp_rpc.rs`.
//!
//! # Spec References
//! - [`SPEC-ANALYZELSP-03~draft`](docs/specs/DOC-72-analyze-on-demand-language-server.md#SPEC-ANALYZELSP-03~draft)
//! - [`SPEC-ANALYZELSP-05~draft`](docs/specs/DOC-72-analyze-on-demand-language-server.md#SPEC-ANALYZELSP-05~draft)

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use tokio::time::Instant;
use trusty_common::control_bus::EventId;
use trusty_common::uds::server::RpcError;

use super::lsp_wire::{
    file_diagnostics, locations, lsp_rpc_error, max_barrier_ms, pulled, BarrierState,
    DiagnosticsScope, FileDiagnostics, LspAnalysedAtRequest, LspAnalysedAtResponse,
    LspDiagnosticsRequest, LspDiagnosticsResponse, LspReferencesRequest, LspReferencesResponse,
};
use crate::lsp::documents::check_version;
use crate::lsp::manager::resolve_workspace_root;
use crate::lsp::snapshot::workspace_snapshot;
use crate::lsp::{LspError, LspLanguage, LspLease, SyncedDocument};
use crate::service::events::AnalyzerAppState;

/// A file synced to its workspace's server, with the workspace stamp.
struct Opened {
    lease: LspLease,
    root: PathBuf,
    file: PathBuf,
    doc: SyncedDocument,
    snapshot: String,
}

fn absolute(path: &Path) -> Result<(), RpcError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(RpcError::invalid_params(format!(
            "`path` must be absolute (got {})",
            path.display()
        )))
    }
}

/// One debug line per call: method, path and causal parent, never content
/// (DOC-72 §4.4). L4 turns the parent into the Started event's `parent_id`.
fn trace_call(method: &str, path: &Path, parent: Option<EventId>) {
    tracing::debug!(method, path = %path.display(), parent = ?parent, "lsp call");
}

/// Lease the server for `path`'s workspace, sync `path` from disk, stamp the
/// snapshot, and refuse a `version` other than the synced one.
async fn open(
    state: &AnalyzerAppState,
    path: &Path,
    version: Option<i64>,
) -> Result<Opened, LspError> {
    if !state.lsp.is_enabled() {
        return Err(LspError::Disabled);
    }
    let root = resolve_workspace_root(path, None)?;
    let language = LspLanguage::for_path(path).ok_or_else(|| LspError::UnsupportedLanguage {
        path: path.display().to_string(),
    })?;
    let file = std::fs::canonicalize(path).map_err(|e| LspError::Document {
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;
    let lease = state.lsp.session(&root, language).await?;
    let doc = lease.client().sync_document(&file, language.id()).await?;
    let snapshot = workspace_snapshot(&root).await?;
    if let Some(v) = version {
        check_version(v, doc.version)?;
    }
    Ok(Opened {
        lease,
        root,
        file,
        doc,
        snapshot,
    })
}

impl Opened {
    /// Wait, inside the request timeout, for the server to finish loading.
    async fn ready(&self) -> Result<(), LspError> {
        let client = self.lease.client();
        client
            .wait_until_quiescent(self.lease.request_timeout())
            .await
    }

    /// Pull this file's diagnostics, bounded by `timeout` (#9078).
    async fn pull(&self, timeout: Duration) -> Result<FileDiagnostics, LspError> {
        let result = self
            .lease
            .request_within(
                "textDocument/diagnostic",
                json!({"textDocument": {"uri": self.doc.uri}}),
                timeout,
            )
            .await?;
        pulled(&self.doc.uri, self.doc.version, &result)
    }
}

/// `analyze.lsp_references`: resolved reference sites at a position.
///
/// # Errors
/// `invalid_params` for a relative path; otherwise a typed LSP refusal
/// (`lsp_not_ready` while the server is still loading).
///
/// Test: `lsp_references_returns_stamped_locations`,
/// `references_wait_for_the_server_to_finish_loading`,
/// `content_modified_is_retried_then_answers`,
/// `a_hung_server_is_reported_through_the_methods`.
pub async fn lsp_references(
    state: &AnalyzerAppState,
    req: LspReferencesRequest,
) -> Result<LspReferencesResponse, RpcError> {
    absolute(&req.path)?;
    trace_call("textDocument/references", &req.path, req.parent_event_id);
    let run = async {
        let o = open(state, &req.path, req.version).await?;
        o.ready().await?;
        let result = o
            .lease
            .request(
                "textDocument/references",
                json!({"textDocument": {"uri": o.doc.uri},
                       "position": {"line": req.line, "character": req.column},
                       "context": {"includeDeclaration": true}}),
            )
            .await?;
        Ok(LspReferencesResponse {
            references: locations(&result)?,
            path: o.file,
            line: req.line,
            column: req.column,
            document_version: o.doc.version,
            snapshot: o.snapshot,
        })
    };
    run.await.map_err(lsp_rpc_error)
}

/// `analyze.lsp_diagnostics`: the server's type-checked diagnostics.
///
/// `scope: file` pulls (`textDocument/diagnostic`) the synced text of `path`
/// once the server is loaded, and answers `complete: true`. rust-analyzer
/// pushes a set only when it changes, so a push can never decide the answer
/// (#9078). `scope: workspace` pulls the same file, then lists every other
/// file the server has pushed for under the root, answering
/// `complete: false`: 1.9.0 runs no full workspace pass, so a directory `path`
/// is refused.
///
/// # Errors
/// `invalid_params` for a missing or relative `path`, or a directory with
/// `scope: workspace`; otherwise a typed LSP refusal (`lsp_not_ready` while the
/// server loads, `lsp_workspace_failed` when it could not load the workspace,
/// `lsp_timeout` when the pull outlives the request timeout).
///
/// Test: `lsp_diagnostics_for_a_file_are_versioned_and_stamped`,
/// `diagnostics_wait_for_the_server_to_finish_loading`,
/// `a_clean_file_opened_during_loading_is_answered_after_quiescence`,
/// `workspace_scope_returns_published_files_marked_incomplete`,
/// `a_directory_workspace_scope_is_refused`.
pub async fn lsp_diagnostics(
    state: &AnalyzerAppState,
    req: LspDiagnosticsRequest,
) -> Result<LspDiagnosticsResponse, RpcError> {
    // #9078: DOC-72 §5 marks `path` optional, but nothing else names a
    // workspace; a guessed root would be a silent fallback.
    let path = req.path.clone().ok_or_else(|| {
        RpcError::invalid_params(
            "`path` is required: the file (scope=file) or a file inside the \
             workspace (scope=workspace)",
        )
    })?;
    absolute(&path)?;
    if req.scope == DiagnosticsScope::Workspace && path.is_dir() {
        return Err(RpcError::invalid_params(
            "workspace pass not supported in 1.9.0: give a file `path`; the answer \
             lists what the server has published so far (complete: false)",
        ));
    }
    trace_call("publishDiagnostics", &path, req.parent_event_id);
    let run = async {
        let o = open(state, &path, req.version).await?;
        o.ready().await?;
        let own = o.pull(o.lease.request_timeout()).await?;
        let files = match req.scope {
            DiagnosticsScope::File => vec![own],
            DiagnosticsScope::Workspace => {
                let mut files: Vec<FileDiagnostics> = published_under(&o.lease, &o.root)?
                    .into_iter()
                    .filter(|f| f.uri != own.uri)
                    .collect();
                files.push(own);
                files.sort_by(|a, b| a.uri.cmp(&b.uri));
                files
            }
        };
        Ok(LspDiagnosticsResponse {
            scope: req.scope,
            path: o.file,
            document_version: o.doc.version,
            snapshot: o.snapshot,
            complete: req.scope == DiagnosticsScope::File,
            files,
        })
    };
    run.await.map_err(lsp_rpc_error)
}

/// Every publication whose file lies under `root`, sorted by URI.
fn published_under(lease: &LspLease, root: &Path) -> Result<Vec<FileDiagnostics>, LspError> {
    let mut all = lease.client().all_diagnostics();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all.iter()
        .filter(|(uri, _)| {
            reqwest::Url::parse(uri)
                .ok()
                .and_then(|u| u.to_file_path().ok())
                .is_some_and(|p| p.starts_with(root))
        })
        .map(|(uri, p)| file_diagnostics(uri, p))
        .collect()
}

/// `analyze.lsp_analysed_at`: block until the loaded server has analysed
/// `version`, proven by a pull (`textDocument/diagnostic`) answered after the
/// sync, as `lsp_diagnostics` does; or until the deadline passes (DOC-72 §3).
///
/// A server still loading at the deadline is `timed_out`: §3 makes the
/// deadline an answer, never an error, and `version_seen` then counts only
/// publications made after the server became quiescent. A server that failed
/// to load the workspace is refused (`lsp_workspace_failed`).
///
/// # Errors
/// `invalid_params` for a relative path or a `timeout_ms` over
/// [`max_barrier_ms`]; `lsp_stale` when `version` is not the file's current
/// version; otherwise a typed LSP refusal.
///
/// Test: `analysed_at_is_ready_once_the_version_is_published`,
/// `analysed_at_times_out_with_the_version_seen`,
/// `analysed_at_accepts_an_unversioned_publication_after_the_sync`,
/// `a_workspace_that_failed_to_load_is_refused`.
pub async fn lsp_analysed_at(
    state: &AnalyzerAppState,
    req: LspAnalysedAtRequest,
) -> Result<LspAnalysedAtResponse, RpcError> {
    absolute(&req.path)?;
    let cap = max_barrier_ms();
    if req.timeout_ms > cap {
        return Err(RpcError::invalid_params(format!(
            "`timeout_ms` is at most {cap} (the MCP client timeout minus 30 s)"
        )));
    }
    trace_call("analysed_at", &req.path, req.parent_event_id);
    let run = async {
        let o = open(state, &req.path, Some(req.version)).await?;
        let client = o.lease.client();
        let deadline = Instant::now() + Duration::from_millis(req.timeout_ms);
        let waited = async {
            let left = deadline.saturating_duration_since(Instant::now());
            client.wait_until_quiescent(left).await?;
            let left = deadline.saturating_duration_since(Instant::now());
            o.pull(left).await
        };
        let state = match waited.await {
            Ok(_) => BarrierState::Ready,
            Err(LspError::Timeout { .. } | LspError::NotReady { .. }) => BarrierState::TimedOut {
                version_seen: client.version_seen(&o.doc.uri),
            },
            Err(e) => return Err(e),
        };
        Ok(LspAnalysedAtResponse {
            state,
            path: o.file,
            document_version: o.doc.version,
            snapshot: o.snapshot,
        })
    };
    run.await.map_err(lsp_rpc_error)
}
