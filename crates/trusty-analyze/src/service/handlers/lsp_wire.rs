//! Wire types for the `analyze.lsp_*` methods, and the LSP-to-wire mapping
//! (#9078, L3).
//!
//! Why: DOC-72 §5 fixes the parameters and §3 the stamps every answer
//! carries; keeping the shapes apart from the handlers keeps both files small
//! and lets the goldens in `tests/lsp_rpc.rs` pin one place.
//! What: request and response structs, LSP `Location`/`Diagnostic` conversion
//! into `path` plus zero-based `line`/`column`, and [`lsp_rpc_error`], which
//! puts every [`LspError`] on the wire as [`CODE_LSP_ERROR`] with a `data`
//! object whose `kind` is the error's stable code.
//! Test: `lsp_references_response_matches_its_golden`,
//! `lsp_diagnostics_response_matches_its_golden`,
//! `a_version_other_than_the_current_one_is_stale`.
//!
//! # Spec References
//! - [`SPEC-ANALYZELSP-05~draft`](docs/specs/DOC-72-analyze-on-demand-language-server.md#SPEC-ANALYZELSP-05~draft)

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use trusty_common::control_bus::EventId;
use trusty_common::uds::server::RpcError;

use crate::lsp::{LspError, PublishedDiagnostics};
use crate::service::events::CODE_LSP_ERROR;

/// Which diagnostics `analyze.lsp_diagnostics` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagnosticsScope {
    /// The one file `path` names.
    File,
    /// Every file the server has published diagnostics for under the root.
    Workspace,
}

/// `analyze.lsp_diagnostics` params: `{scope, path?, version?, parent_event_id?}`.
#[derive(Debug, Clone, Deserialize)]
pub struct LspDiagnosticsRequest {
    /// `file` or `workspace`.
    pub scope: DiagnosticsScope,
    /// Absolute. Required in practice: the file (`file`), or any file or
    /// directory inside the workspace (`workspace`); see the handler.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Refused as `lsp_stale` unless it is the file's current version.
    #[serde(default)]
    pub version: Option<i64>,
    /// Causal parent for the L4 events (DOC-72 §4.3); validated, not used yet.
    #[serde(default)]
    pub parent_event_id: Option<EventId>,
}

/// `analyze.lsp_references` params: `{path, line, column, version?, parent_event_id?}`.
#[derive(Debug, Clone, Deserialize)]
pub struct LspReferencesRequest {
    /// Absolute path of the file holding the position.
    pub path: PathBuf,
    /// Zero-based line, as in LSP.
    pub line: u32,
    /// Zero-based column (LSP `character`).
    pub column: u32,
    /// Refused as `lsp_stale` unless it is the file's current version.
    #[serde(default)]
    pub version: Option<i64>,
    /// Causal parent for the L4 events (DOC-72 §4.3); validated, not used yet.
    #[serde(default)]
    pub parent_event_id: Option<EventId>,
}

/// `analyze.lsp_analysed_at` params: `{path, version, timeout_ms, parent_event_id?}`.
///
/// `path` is not in DOC-72 §5's row; a document version is per file, so the
/// barrier needs to know which file (recorded in the PR body).
#[derive(Debug, Clone, Deserialize)]
pub struct LspAnalysedAtRequest {
    /// Absolute path of the file whose version is awaited.
    pub path: PathBuf,
    /// The version to wait for; any other current version is `lsp_stale`.
    pub version: i64,
    /// The server-side deadline, at most [`max_barrier_ms`].
    pub timeout_ms: u64,
    /// Causal parent for the L4 events (DOC-72 §4.3); validated, not used yet.
    #[serde(default)]
    pub parent_event_id: Option<EventId>,
}

/// Headroom between the longest barrier and the MCP client's own timeout.
const BARRIER_MARGIN: Duration = Duration::from_secs(30);

/// The longest barrier a caller may ask for: the MCP client timeout
/// (`core::deadlines::mcp_client_timeout`, 270 s by default) minus 30 s, so an
/// MCP caller always reads the barrier's answer rather than its own timeout.
///
/// Test: `malformed_params_are_invalid_params`.
pub fn max_barrier_ms() -> u64 {
    let cap = crate::core::deadlines::mcp_client_timeout().saturating_sub(BARRIER_MARGIN);
    u64::try_from(cap.as_millis()).unwrap_or(u64::MAX)
}

/// A zero-based position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Position {
    pub line: u64,
    pub column: u64,
}

/// One resolved reference site.
#[derive(Debug, Clone, Serialize)]
pub struct Location {
    /// The file, when the URI is a `file://` one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub uri: String,
    pub start: Position,
    pub end: Position,
}

/// What `analyze.lsp_references` answers with.
#[derive(Debug, Clone, Serialize)]
pub struct LspReferencesResponse {
    pub path: PathBuf,
    pub line: u32,
    pub column: u32,
    /// The version of `path` the answer was computed against.
    pub document_version: i64,
    /// `<git head>+<dirty hash>` of the workspace.
    pub snapshot: String,
    pub references: Vec<Location>,
}

/// One diagnostic. The message travels in the RPC answer only (DOC-72 §4.4).
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub start: Position,
    pub end: Position,
    pub severity: Option<u64>,
    pub code: Option<Value>,
    pub source: Option<String>,
    pub message: String,
}

/// The latest diagnostics the server published for one file.
#[derive(Debug, Clone, Serialize)]
pub struct FileDiagnostics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub uri: String,
    /// The document version the server stamped, if it stamped one.
    pub version: Option<i64>,
    /// True when the server sent no version (architect default (b)); such a
    /// publication is accepted and never refused as stale.
    pub unversioned: bool,
    pub diagnostics: Vec<Diagnostic>,
}

/// What `analyze.lsp_diagnostics` answers with.
#[derive(Debug, Clone, Serialize)]
pub struct LspDiagnosticsResponse {
    pub scope: DiagnosticsScope,
    pub path: PathBuf,
    /// The version of `path`.
    pub document_version: i64,
    pub snapshot: String,
    /// True only for `scope: file` with both the pull and a push describing
    /// this version (#9078). Always false for `scope: workspace`, which lists
    /// only the files the server has published for so far; 1.9.0 runs no full
    /// workspace pass.
    pub complete: bool,
    /// Why `complete` is false: `check_not_run` (no `cargo check` push for
    /// covers this version: one runs only on the opening version) or
    /// `published_files_only` (workspace scope).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    pub files: Vec<FileDiagnostics>,
}

/// The barrier's answer (DOC-72 §3): it never reports a partial state.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BarrierState {
    /// The server has published diagnostics at the requested version.
    Ready,
    /// The deadline passed; `version_seen` is the newest stamped publication.
    TimedOut { version_seen: Option<i64> },
}

/// What `analyze.lsp_analysed_at` answers with.
#[derive(Debug, Clone, Serialize)]
pub struct LspAnalysedAtResponse {
    #[serde(flatten)]
    pub state: BarrierState,
    pub path: PathBuf,
    pub document_version: i64,
    pub snapshot: String,
}

/// A malformed answer, described by its expected shape; the value itself may
/// hold source text and is never echoed (DOC-72 §4.4).
fn protocol(what: &str, expected: &str) -> LspError {
    LspError::Protocol(format!(
        "malformed {what} from the server: expected {expected}"
    ))
}

fn position(v: &Value) -> Option<Position> {
    Some(Position {
        line: v.get("line")?.as_u64()?,
        column: v.get("character")?.as_u64()?,
    })
}

fn range(v: &Value) -> Option<(Position, Position)> {
    let r = v.get("range")?;
    Some((position(r.get("start")?)?, position(r.get("end")?)?))
}

fn uri_path(uri: &str) -> Option<String> {
    let url = reqwest::Url::parse(uri).ok()?;
    let path = url.to_file_path().ok()?;
    Some(path.display().to_string())
}

/// Convert a `textDocument/references` result (`Location[] | null`).
///
/// # Errors
/// [`LspError::Protocol`] for anything else; nothing is dropped silently.
pub fn locations(result: &Value) -> Result<Vec<Location>, LspError> {
    let items = match result {
        Value::Null => return Ok(Vec::new()),
        Value::Array(items) => items,
        _ => return Err(protocol("references result", "Location[] or null")),
    };
    items
        .iter()
        .map(|item| {
            let uri = item.get("uri").and_then(Value::as_str);
            match (uri, range(item)) {
                (Some(uri), Some((start, end))) => Ok(Location {
                    path: uri_path(uri),
                    uri: uri.to_string(),
                    start,
                    end,
                }),
                _ => Err(protocol("location", "{uri, range}")),
            }
        })
        .collect()
}

/// Convert one publication for `uri`.
///
/// # Errors
/// [`LspError::Protocol`] for a diagnostic without a range or a message.
pub fn file_diagnostics(uri: &str, p: &PublishedDiagnostics) -> Result<FileDiagnostics, LspError> {
    let diagnostics = p
        .diagnostics
        .iter()
        .map(|d| {
            let message = d.get("message").and_then(Value::as_str);
            match (range(d), message) {
                (Some((start, end)), Some(message)) => Ok(Diagnostic {
                    start,
                    end,
                    severity: d.get("severity").and_then(Value::as_u64),
                    code: d.get("code").cloned(),
                    source: d.get("source").and_then(Value::as_str).map(str::to_string),
                    message: message.to_string(),
                }),
                _ => Err(protocol("diagnostic", "{range, message}")),
            }
        })
        .collect::<Result<_, _>>()?;
    Ok(FileDiagnostics {
        path: uri_path(uri),
        uri: uri.to_string(),
        version: p.version,
        unversioned: p.version.is_none(),
        diagnostics,
    })
}

/// Merge a `textDocument/diagnostic` (pull) answer for `uri` at `version` with
/// the push held for the same URI (#9078).
///
/// Why: rust-analyzer's pull carries only its native analysis; `cargo check`
/// (flycheck) findings such as E0382 arrive only as a push. Either half alone
/// is not the file's diagnostics.
/// What: the pull is ordered after the sync on the server's input, so its
/// items describe `version`. The push is added when it describes the same
/// text (rule below); items present in both are listed once. Returns the
/// entry, stamped `version`, and whether the push half was present.
///
/// # Errors
/// [`LspError::Protocol`] for anything but a `full` report with `items`, or a
/// malformed diagnostic.
///
/// Test: `file_scope_merges_the_pull_with_the_check_push`,
/// `with_no_check_push_yet_the_answer_is_incomplete`,
/// `a_late_check_stamped_with_a_newer_version_is_not_counted`,
/// `a_changed_file_is_pulled_at_its_new_version`.
pub fn merged(
    uri: &str,
    version: i64,
    synced_at: u64,
    pull: &Value,
    push: Option<&PublishedDiagnostics>,
) -> Result<(FileDiagnostics, bool), LspError> {
    let mut items = match (pull.get("kind").and_then(Value::as_str), pull.get("items")) {
        (Some("full"), Some(Value::Array(items))) => items.clone(),
        _ => return Err(protocol("diagnostic report", "{kind: full, items}")),
    };
    // #9078: a check push counts only for the OPENING version. rust-analyzer
    // stamps a push with the version current at PUBLISH time, and `cargo
    // check` runs only at startup (the daemon sends no didSave), so a check
    // that read v1 can land stamped v2; only v1 text is ever checked. So a
    // push counts when `version` is 1 and it is stamped 1, or unstamped and
    // arrived after v1's text was sent (`seq > synced_at`). Any other push may
    // describe other text and is left out: the answer is incomplete, not wrong.
    let check = push.filter(|p| {
        version == 1
            && match p.version {
                Some(v) => v == 1,
                None => p.seq > synced_at,
            }
    });
    if let Some(p) = check {
        for d in &p.diagnostics {
            if !items.contains(d) {
                items.push(d.clone());
            }
        }
    }
    let report = PublishedDiagnostics {
        version: Some(version),
        diagnostics: items,
        seq: 0,
    };
    Ok((file_diagnostics(uri, &report)?, check.is_some()))
}

/// Put an [`LspError`] on the wire: [`CODE_LSP_ERROR`], the error's text, and
/// `data = {kind, ...}` with the fields a caller branches on.
///
/// Test: `a_version_other_than_the_current_one_is_stale`,
/// `a_hung_server_is_reported_through_the_methods`,
/// `disabled_flag_is_lsp_disabled_for_every_method`.
pub fn lsp_rpc_error(e: LspError) -> RpcError {
    let mut data = json!({"kind": e.code()});
    let extra = match &e {
        LspError::Stale { requested, current } => {
            json!({"requested": requested, "current": current})
        }
        LspError::Timeout { method, timeout_ms } => {
            json!({"method": method, "timeout_ms": timeout_ms})
        }
        LspError::Hung { consecutive } => json!({"consecutive": consecutive}),
        LspError::NotReady { waited_ms } => json!({"waited_ms": waited_ms}),
        LspError::ServerError { code, .. } => json!({"server_code": code}),
        LspError::NotInstalled { program } | LspError::SpawnFailed { program, .. } => {
            json!({"program": program})
        }
        LspError::FileNotFound { path }
        | LspError::OutsideWorkspace { path }
        | LspError::UnsupportedLanguage { path }
        | LspError::Document { path, .. } => json!({"path": path}),
        _ => json!({}),
    };
    if let (Some(d), Value::Object(extra)) = (data.as_object_mut(), extra) {
        d.extend(extra);
    }
    RpcError::new(CODE_LSP_ERROR, e.to_string()).with_data(data)
}
