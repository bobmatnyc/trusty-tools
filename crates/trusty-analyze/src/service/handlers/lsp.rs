//! `analyze.lsp_diagnostics`, `analyze.lsp_references` and
//! `analyze.lsp_analysed_at` (#9078, L3).
//!
//! Why: DOC-72 §5 serves type-checked answers from the on-demand language
//! server on the analyze socket. §2 forbids failing open and §3 requires every
//! answer to name the document version and workspace snapshot it describes.
//! What: each handler checks the default-off flag, resolves the file's
//! workspace root, leases that root's server (spawning it if needed), syncs the
//! file from disk, refuses a stale `version`, asks the server, and stamps the
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
use trusty_common::control_bus::EventId;
use trusty_common::uds::server::RpcError;

use super::lsp_wire::{
    file_diagnostics, locations, lsp_rpc_error, BarrierState, DiagnosticsScope, FileDiagnostics,
    LspAnalysedAtRequest, LspAnalysedAtResponse, LspDiagnosticsRequest, LspDiagnosticsResponse,
    LspReferencesRequest, LspReferencesResponse, MAX_BARRIER_MS,
};
use crate::lsp::documents::check_version;
use crate::lsp::manager::resolve_workspace_root;
use crate::lsp::snapshot::workspace_snapshot;
use crate::lsp::{LspError, LspLanguage, LspLease, SyncedDocument};
use crate::service::events::AnalyzerAppState;

/// A file synced to its workspace's server.
struct Opened {
    lease: LspLease,
    root: PathBuf,
    file: PathBuf,
    doc: SyncedDocument,
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

fn ensure_enabled(state: &AnalyzerAppState) -> Result<(), LspError> {
    if state.lsp.is_enabled() {
        Ok(())
    } else {
        Err(LspError::Disabled)
    }
}

/// Lease the server for `path`'s workspace and sync `path` from disk.
async fn open(state: &AnalyzerAppState, path: &Path) -> Result<Opened, LspError> {
    ensure_enabled(state)?;
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
    Ok(Opened {
        lease,
        root,
        file,
        doc,
    })
}

/// `analyze.lsp_references`: resolved reference sites at a position.
///
/// # Errors
/// `invalid_params` for a relative path; otherwise a typed LSP refusal.
///
/// Test: `lsp_references_returns_stamped_locations`,
/// `content_modified_is_retried_then_answers`,
/// `a_hung_server_is_reported_through_the_methods`.
pub async fn lsp_references(
    state: &AnalyzerAppState,
    req: LspReferencesRequest,
) -> Result<LspReferencesResponse, RpcError> {
    absolute(&req.path)?;
    trace_call("textDocument/references", &req.path, req.parent_event_id);
    let run = async {
        let o = open(state, &req.path).await?;
        if let Some(v) = req.version {
            check_version(v, o.doc.version)?;
        }
        let snapshot = workspace_snapshot(&o.root).await?;
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
            path: o.file,
            line: req.line,
            column: req.column,
            document_version: o.doc.version,
            snapshot,
            references: locations(&result)?,
        })
    };
    run.await.map_err(lsp_rpc_error)
}

/// `analyze.lsp_diagnostics`: the server's type-checked diagnostics.
///
/// `scope: file` waits for diagnostics describing the synced text of `path`.
/// `scope: workspace` does the same when `path` is a file, then returns every
/// file published under the root; a directory `path` syncs nothing, takes no
/// `version`, and returns what the server has published so far.
///
/// # Errors
/// `invalid_params` for a missing or relative `path`, or a `version` with a
/// directory; otherwise a typed LSP refusal (`lsp_timeout` when the server
/// publishes nothing for the file within the request timeout).
///
/// Test: `lsp_diagnostics_for_a_file_are_versioned_and_stamped`,
/// `unversioned_diagnostics_are_accepted_and_never_stale`,
/// `workspace_scope_returns_every_published_file_under_the_root`.
pub async fn lsp_diagnostics(
    state: &AnalyzerAppState,
    req: LspDiagnosticsRequest,
) -> Result<LspDiagnosticsResponse, RpcError> {
    // #9078: DOC-72 §5 marks `path` optional, but nothing else names a
    // workspace; a guessed root would be a silent fallback.
    let path = req.path.clone().ok_or_else(|| {
        RpcError::invalid_params(
            "`path` is required: the file (scope=file) or any file or directory \
             inside the workspace (scope=workspace)",
        )
    })?;
    absolute(&path)?;
    trace_call("publishDiagnostics", &path, req.parent_event_id);
    if req.scope == DiagnosticsScope::Workspace && path.is_dir() {
        if req.version.is_some() {
            return Err(RpcError::invalid_params(
                "`version` names a document version; give a file `path`",
            ));
        }
        return workspace_dir(state, &path).await.map_err(lsp_rpc_error);
    }
    let run = async {
        let o = open(state, &path).await?;
        if let Some(v) = req.version {
            check_version(v, o.doc.version)?;
        }
        let client = o.lease.client();
        let fresh = client
            .wait_for_fresh_diagnostics(
                &o.doc.uri,
                o.doc.version,
                o.doc.synced_at,
                o.lease.request_timeout(),
            )
            .await?;
        let files = match req.scope {
            DiagnosticsScope::File => vec![file_diagnostics(&o.doc.uri, &fresh)?],
            DiagnosticsScope::Workspace => published_under(&o.lease, &o.root)?,
        };
        Ok(LspDiagnosticsResponse {
            scope: req.scope,
            path: o.file,
            document_version: Some(o.doc.version),
            snapshot: workspace_snapshot(&o.root).await?,
            files,
        })
    };
    run.await.map_err(lsp_rpc_error)
}

/// `scope: workspace` with a directory: what has been published under it.
async fn workspace_dir(
    state: &AnalyzerAppState,
    dir: &Path,
) -> Result<LspDiagnosticsResponse, LspError> {
    ensure_enabled(state)?;
    let root = resolve_workspace_root(dir, None)?;
    let dir = std::fs::canonicalize(dir).map_err(|e| LspError::Document {
        path: dir.display().to_string(),
        reason: e.to_string(),
    })?;
    // 1.9.0 configures Rust only (DOC-72 §12, D2).
    let lease = state.lsp.session(&root, LspLanguage::Rust).await?;
    Ok(LspDiagnosticsResponse {
        scope: DiagnosticsScope::Workspace,
        path: dir,
        document_version: None,
        snapshot: workspace_snapshot(&root).await?,
        files: published_under(&lease, &root)?,
    })
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

/// `analyze.lsp_analysed_at`: block until the server has published
/// diagnostics at `version`, or the deadline passes (DOC-72 §3).
///
/// # Errors
/// `invalid_params` for a relative path or a `timeout_ms` over
/// [`MAX_BARRIER_MS`]; `lsp_stale` when `version` is not the file's current
/// version; otherwise a typed LSP refusal. A deadline is an answer
/// (`timed_out`), not an error.
///
/// Test: `analysed_at_is_ready_once_the_version_is_published`,
/// `analysed_at_times_out_with_the_version_seen`.
pub async fn lsp_analysed_at(
    state: &AnalyzerAppState,
    req: LspAnalysedAtRequest,
) -> Result<LspAnalysedAtResponse, RpcError> {
    absolute(&req.path)?;
    if req.timeout_ms > MAX_BARRIER_MS {
        return Err(RpcError::invalid_params(format!(
            "`timeout_ms` is at most {MAX_BARRIER_MS}"
        )));
    }
    trace_call("analysed_at", &req.path, req.parent_event_id);
    let run = async {
        let o = open(state, &req.path).await?;
        check_version(req.version, o.doc.version)?;
        let client = o.lease.client();
        let wait = Duration::from_millis(req.timeout_ms);
        let state = match client
            .wait_for_diagnostics(&o.doc.uri, req.version, wait)
            .await
        {
            Ok(_) => BarrierState::Ready,
            Err(LspError::Timeout { .. }) => BarrierState::TimedOut {
                version_seen: client.diagnostics(&o.doc.uri).and_then(|d| d.version),
            },
            Err(e) => return Err(e),
        };
        Ok(LspAnalysedAtResponse {
            state,
            path: o.file,
            document_version: o.doc.version,
            snapshot: workspace_snapshot(&o.root).await?,
        })
    };
    run.await.map_err(lsp_rpc_error)
}
