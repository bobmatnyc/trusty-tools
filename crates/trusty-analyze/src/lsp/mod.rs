//! The on-demand language-server client and its child manager (#9078, L2).
//!
//! Why: DOC-72 moves type-checked semantics into trusty-analyze, one language
//! server per workspace, spawned on demand and killed when idle. The shared
//! `UdsServiceSupervisor` cannot host it: a language server binds no socket and
//! speaks LSP over stdio (DOC-72 §2).
//! What: [`framing`] is the `Content-Length` codec; [`LspClient`] correlates
//! JSON-RPC requests and responses over any byte pipe and versions the
//! documents it sends ([`documents`]); [`child`] spawns one server in its own
//! process group and reaps it; [`LspManager`] keys children by
//! `(workspace root, language)` behind the default-off flag in [`config`].
//! Every failure is a typed [`LspError`]; nothing degrades to an empty answer.
//! Test: `src/lsp/tests.rs` (codec, client, documents, config) and
//! `tests/lsp_child.rs` (spawn, crash, hang, idle reap, leaks).
//!
//! # Spec References
//! - [`SPEC-ANALYZELSP-02~draft`](docs/specs/DOC-72-analyze-on-demand-language-server.md#SPEC-ANALYZELSP-02~draft)
//! - [`SPEC-ANALYZELSP-03~draft`](docs/specs/DOC-72-analyze-on-demand-language-server.md#SPEC-ANALYZELSP-03~draft)

pub mod child;
pub mod client;
pub mod config;
pub mod documents;
pub mod framing;
pub(crate) mod group;
pub mod manager;
pub mod snapshot;

pub use child::LspChild;
pub use client::{LspClient, PublishedDiagnostics};
pub use config::{LspCommand, LspConfig, LspLanguage};
pub use documents::SyncedDocument;
pub use manager::{LspLease, LspManager};

/// Every way an LSP call can fail (#9078).
///
/// Why: DOC-72 §2 forbids failing open. A missing binary, a crash or a timeout
/// must reach the caller as a refusal it can name, never as an empty result.
/// What: `Clone` so the manager can replay a recorded failure during its
/// cool-off; [`LspError::code`] is the stable string the `analyze.lsp_*`
/// methods (L3) put on the wire.
/// Test: `every_error_has_a_stable_code`, `missing_binary_is_not_installed`,
/// `failed_initialize_carries_the_stderr_tail`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LspError {
    /// The capability flag is off (DOC-72 §2, default D10).
    #[error("the LSP capability is disabled; set ANALYZE_LSP=1 to enable it")]
    Disabled,
    /// The server binary is not on `PATH` or not executable.
    #[error("language server `{program}` is not installed or not executable")]
    NotInstalled { program: String },
    /// The binary exists but could not be started.
    #[error("failed to spawn language server `{program}`: {reason}")]
    SpawnFailed { program: String, reason: String },
    /// `initialize` failed; `stderr_tail` is the server's last stderr lines.
    #[error("language server failed to initialize: {reason}; stderr tail: {stderr_tail}")]
    InitializeFailed { reason: String, stderr_tail: String },
    /// One request outlived its deadline; the server may still be usable.
    #[error("`{method}` timed out after {timeout_ms} ms")]
    Timeout { method: String, timeout_ms: u64 },
    /// The server closed its stdout or exited.
    #[error("language server exited: {detail}")]
    ServerExited { detail: String },
    /// Too many consecutive timeouts; the server was killed.
    #[error("language server hung ({consecutive} consecutive timeouts) and was killed")]
    Hung { consecutive: u32 },
    /// A malformed frame; the stream cannot be resynchronised.
    #[error("malformed LSP frame: {0}")]
    Protocol(String),
    /// The server answered with a JSON-RPC error object.
    #[error("language server error {code}: {message}")]
    ServerError { code: i64, message: String },
    /// Writing to the server's stdin failed.
    #[error("write to language server failed: {0}")]
    Write(String),
    /// A file could not be read, located or addressed.
    #[error("document {path}: {reason}")]
    Document { path: String, reason: String },
    /// The caller named a document version the server is not at (DOC-72 §3).
    #[error("stale document version: requested {requested}, server is at {current}")]
    Stale { requested: i64, current: i64 },
    /// The named file does not exist (#9078, L3).
    #[error("{path}: no such file")]
    FileNotFound { path: String },
    /// The file is under no workspace root: no index root, no git work tree.
    #[error("{path}: not under any workspace root (no enclosing git work tree)")]
    OutsideWorkspace { path: String },
    /// No language server is configured for this file's language.
    #[error("{path}: no language server for this file type (1.9.0 serves Rust only)")]
    UnsupportedLanguage { path: String },
    /// The server did not report itself quiescent in time: it is still
    /// loading, and its answers would be empty rather than true (#9078).
    #[error("language server still loading after {waited_ms} ms; ask again later")]
    NotReady { waited_ms: u64 },
    /// The `<git head>+<dirty hash>` stamp could not be computed (DOC-72 §3).
    #[error("workspace snapshot of {root}: {reason}")]
    Snapshot { root: String, reason: String },
}

impl LspError {
    /// The stable machine-readable code for this error.
    ///
    /// Test: `every_error_has_a_stable_code`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "lsp_disabled",
            Self::NotInstalled { .. } => "lsp_not_installed",
            Self::SpawnFailed { .. } => "lsp_spawn_failed",
            Self::InitializeFailed { .. } => "lsp_initialize_failed",
            Self::Timeout { .. } => "lsp_timeout",
            Self::ServerExited { .. } => "lsp_server_exited",
            Self::Hung { .. } => "lsp_hung",
            Self::Protocol(_) => "lsp_protocol",
            Self::ServerError { .. } => "lsp_server_error",
            Self::Write(_) => "lsp_write_failed",
            Self::Document { .. } => "lsp_document",
            Self::Stale { .. } => "lsp_stale",
            Self::FileNotFound { .. } => "lsp_file_not_found",
            Self::OutsideWorkspace { .. } => "lsp_outside_workspace",
            Self::UnsupportedLanguage { .. } => "lsp_unsupported_language",
            Self::NotReady { .. } => "lsp_not_ready",
            Self::Snapshot { .. } => "lsp_snapshot_failed",
        }
    }
}

/// Lock a std mutex, recovering the data if a holder panicked.
///
/// Why: every guarded value here (waiter tables, diagnostics, timestamps) stays
/// consistent across a panic, and the crate bans `unwrap()` in library code.
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
