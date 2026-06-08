//! Request/response wire types shared across handler modules.
//!
//! Why: Public request/response structs (`CreateIndexRequest`, `IndexFileRequest`,
//! `RemoveFileRequest`) used by multiple callers are defined here and
//! re-exported through `mod.rs` to preserve the original public surface.
//! What: All `#[derive(Deserialize/Serialize)]` types that cross module
//! boundaries. Private response types local to one handler live in that
//! handler's module instead.
//! Test: exercised by each endpoint's handler tests.
use serde::{Deserialize, Serialize};

/// Response shape for `GET /indexes` (flat list).
///
/// Why: Backward-compatible flat list returned when `?details` is absent.
/// What: `{"indexes":["id1","id2"]}`.
/// Test: `list_indexes_returns_empty_initially` and related.
#[derive(Serialize)]
pub(super) struct IndexListResponse {
    pub indexes: Vec<String>,
}

/// Per-index entry returned by `GET /indexes?details=true` (issue #312).
///
/// Why: the flat list (`GET /indexes`) returns bare id strings for backward
/// compatibility; adding `?details=true` returns richer objects so the MCP
/// `list_indexes` tool and UI can display per-index disk usage without a
/// separate round-trip.  `root_path` is also exposed here so callers (e.g.
/// trusty-review's auto-derive logic, issue #661) can match an index to the
/// current project directory without a separate per-index status round-trip.
/// What: `id` + `root_path` (canonical absolute path stored on the handle) +
/// `size_bytes` (sum of all file sizes under the index data directory; `null`
/// when the directory has not been created yet).
/// Test: `list_indexes_details_includes_size_bytes`,
/// `list_indexes_details_includes_root_path`.
#[derive(serde::Serialize)]
pub(super) struct IndexDetailEntry {
    pub id: String,
    /// Canonical absolute path of the indexed directory.
    ///
    /// Why: trusty-review and other callers need to map the current project
    /// root to an index without issuing N status requests (issue #661).
    /// What: the `root_path` stored on the `IndexHandle` at registration time,
    /// serialised as a UTF-8 string (lossless on all supported platforms).
    /// Absent (null) only when the handle's path cannot be converted to UTF-8
    /// (a practically impossible edge case on macOS / Linux).
    /// Test: `list_indexes_details_includes_root_path`.
    pub root_path: Option<String>,
    pub size_bytes: Option<u64>,
}

#[derive(Deserialize)]
pub struct CreateIndexRequest {
    pub id: String,
    pub root_path: std::path::PathBuf,
    /// Subtrees (relative to `root_path`) to restrict indexing to. Forwarded
    /// from `trusty-search.yaml`'s `paths:` field by `trusty-search index`.
    /// Empty / missing = walk the entire `root_path`.
    #[serde(default)]
    pub include_paths: Option<Vec<String>>,
    /// Glob patterns to exclude on top of the built-in ignores.
    #[serde(default)]
    pub exclude_globs: Option<Vec<String>>,
    /// Extension allow-list (e.g. `["rs", "py"]`, without leading dot).
    #[serde(default)]
    pub extensions: Option<Vec<String>>,
    /// Domain vocabulary for the per-index intent classifier.
    #[serde(default)]
    pub domain_terms: Option<Vec<String>>,
    /// Glob patterns (issue #111) matched against the immediate subdirectory
    /// name under `root_path`. When non-empty, only files inside subdirectories
    /// whose basename matches at least one pattern are indexed. Supports `*`
    /// wildcards (no `**`). Distinct from `include_paths` (absolute subtrees
    /// from `trusty-search.yaml`) — `path_filter` is the API-level glob filter
    /// intended for filtering polyrepo monorepos by repo-name pattern.
    #[serde(default)]
    pub path_filter: Option<Vec<String>>,
    /// Index prose docs (`*.md`, `*.rst`, README, CHANGELOG, …) —
    /// issues #77 and #118. Default `None` is now treated as `true` (was
    /// `false` through v0.8.2); the per-mode filter
    /// (`is_allowed_for_mode`) keeps these chunks out of `mode=code`
    /// results, but `mode=text` needs them indexed at all. Set `false`
    /// from `trusty-search.yaml` only when the operator genuinely does
    /// not want any prose chunked (saves chunks on docs-heavy projects).
    #[serde(default)]
    pub include_docs: Option<bool>,
    /// Honour `.gitignore` (plus `.ignore`, `.rgignore`, `.git/info/exclude`,
    /// global gitignore) during the reindex walk — issue #100. Default
    /// `None` (treated as `true` by the daemon, matching ripgrep semantics).
    /// Set `false` from `trusty-search.yaml` when the operator wants to
    /// index a gitignored / vendored subtree on purpose.
    ///
    /// Why: previously the walker used `walkdir` and ignored `.gitignore`,
    /// which combined with the chunk budget caused silent partial-index
    /// failures — a gitignored subtree dominated the budget before the
    /// walker reached the real source. Exposing the toggle on the wire keeps
    /// the opt-out reachable for callers that need it.
    #[serde(default)]
    pub respect_gitignore: Option<bool>,
    /// Staged-pipeline opt-out (issue #109, Phase 1). When `true`, the
    /// reindex pipeline returns after Stage 1 (lexical / BM25 / redb) and
    /// permanently skips the embedder + symbol-graph stages. Useful for
    /// callers who want a daemonized ripgrep without the embedder overhead.
    /// Persisted to `indexes.toml` so the choice survives daemon restarts.
    /// Default `None` (treated as `false` — full pipeline).
    #[serde(default)]
    pub lexical_only: Option<bool>,

    /// Stage-1-minimal mode (issue #313): when `true`, the Phase 3 KG
    /// rebuild is skipped entirely during reindex. The graph stage is
    /// permanently `Skipped`. `get_call_chain` and `search_kg` return a
    /// 503 `kg_unavailable` error. Orthogonal to `lexical_only`.
    /// Default `None` (treated as `false` — KG is built as normal).
    ///
    /// Why: exposes the per-index KG-suppression flag on the wire so callers
    /// can register a skip_kg index in one `POST /indexes` call.
    /// What: `None` maps to `false`; `true` is stored in `indexes.toml` and
    /// survives daemon restarts.
    /// Test: `skip_kg_index_never_runs_phase3` in `service::reindex::tests`.
    #[serde(default)]
    pub skip_kg: Option<bool>,

    /// Deferred-embedding opt-out (issue #923). When `None` or `Some(true)`
    /// (the default), the fast pass runs synchronously and marks lexical + graph
    /// `Ready` within seconds; semantic embedding is deferred to a background
    /// job. Set `Some(false)` to force the old synchronous full index — semantic
    /// is `Ready` before the reindex call returns.
    ///
    /// Why: exposes the per-index `defer_embed` flag on the wire so callers can
    /// opt out of deferred embedding when they need semantic search available
    /// immediately (e.g. CI pipelines that query right after indexing).
    /// What: `None` / missing maps to `true` (deferred is the new default).
    /// Test: `defer_embed_false_forces_synchronous_index` in `service::reindex::tests`.
    #[serde(default)]
    pub defer_embed: Option<bool>,
}

#[derive(Deserialize)]
pub struct IndexFileRequest {
    pub path: String,
    pub content: String,
}

#[derive(Deserialize)]
pub struct RemoveFileRequest {
    pub path: String,
}
