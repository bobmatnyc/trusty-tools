use crate::core::indexer::CodeIndexer;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Staged-pipeline lifecycle status of a single indexing stage (issue #109,
/// Phase 1).
///
/// Why: the staged pipeline splits a reindex into three logical stages
/// (lexical → semantic → graph). Callers need to see per-stage progress so a
/// search can opt into the lanes that are ready without blocking on the
/// embedder. A coarse-grained `Pending | InProgress | Ready | Skipped` keeps
/// the wire payload tiny and is enough to drive graceful degradation in the
/// search handler.
/// What: a four-variant enum serialised in snake_case (`pending`,
/// `in_progress`, `ready`, `skipped`) so the JSON wire format matches the
/// ticket spec exactly.
/// Test: `service::reindex::tests::stage_states_advance_through_pipeline`
/// (Phase 1) and the e2e tests under `core::registry::tests`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    /// Stage has not started yet (default on registry).
    #[default]
    Pending,
    /// Stage is currently running.
    InProgress,
    /// Stage completed successfully.
    Ready,
    /// Stage was deliberately skipped (e.g. `--lexical-only` opts out of
    /// stages 2 and 3 permanently). Distinct from `Pending`/`Ready` so
    /// callers can tell "never going to happen" from "not started yet".
    Skipped,
    /// Issue #601: the stage failed and its results are NOT queryable. Used by
    /// the reindex non-empty gate when a full-pipeline index walked files but
    /// the embedder produced zero vectors (silent embed failure). Distinct from
    /// `Skipped` (deliberate opt-out) and `Pending` (not started) so callers —
    /// and `/health` / `GET /indexes/:id` — can surface a LOUD failure instead
    /// of a false-green ready state.
    Failed,
}

impl StageStatus {
    /// True when this stage's results are queryable.
    pub fn is_ready(self) -> bool {
        matches!(self, StageStatus::Ready)
    }
}

/// Live per-stage state surfaced on `IndexHandle::stages`.
///
/// Why: the staged-pipeline status surface (issue #109) needs a place to
/// store per-stage timing + counters that the search handler can consult to
/// decide which lanes are available. Wrapped in `Arc<RwLock<>>` on the
/// handle so reindex tasks can flip the state without rebuilding the entry.
/// What: a tiny struct carrying the status plus optional started/completed
/// RFC-3339 timestamps and stage-specific counters. All fields are optional
/// so callers can read partial progress mid-reindex.
/// Test: covered by the staged-pipeline e2e tests in `service::reindex`.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct StageState {
    pub status: StageStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// Files processed (lexical stage). `None` when the field is not
    /// applicable to this stage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<usize>,
    /// Chunks committed to the lexical stage. Surfaced as `chunks` in the
    /// JSON payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunks: Option<usize>,
    /// Embedded chunks (semantic stage running counter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedded: Option<usize>,
    /// Total chunks to embed (semantic stage denominator).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    /// Issue #601: human-readable failure reason when `status == Failed`.
    /// `None` for every other state. Surfaced on `GET /indexes/:id` so an
    /// operator sees WHY a stage failed without reading daemon logs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

impl StageState {
    /// Build a fresh `Pending` stage.
    pub fn pending() -> Self {
        Self::default()
    }

    /// Build a stage that is permanently `Skipped` (used by `lexical_only`).
    pub fn skipped() -> Self {
        Self {
            status: StageStatus::Skipped,
            ..Self::default()
        }
    }

    /// Build a `Failed` stage carrying `reason` (issue #601). The
    /// `completed_at` timestamp records when the failure was detected.
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            status: StageStatus::Failed,
            failure: Some(reason.into()),
            ..Self::default()
        }
    }
}

/// Per-index staged-pipeline snapshot. Three stages, in dependency order:
/// `lexical` → `semantic` → `graph`. The search handler reads this to derive
/// `search_capabilities` and gate lane participation.
///
/// Why: the v0.9.0 staged-pipeline refactor (issue #109) decouples the
/// synchronous lexical lane from the heavier embedder + graph stages. A
/// single struct holding all three states is cheaper to read than three
/// separate `Arc<RwLock<>>`s and keeps the JSON status payload coherent.
/// What: three `StageState` fields, serialised in the order the ticket spec
/// expects.
/// Test: `stage_status_capabilities_*` in this module.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct IndexStages {
    pub lexical: StageState,
    pub semantic: StageState,
    pub graph: StageState,
}

impl IndexStages {
    /// Compute the public `search_capabilities` array from the per-stage
    /// statuses. Mirrors the wire schema documented on the ticket:
    /// `["bm25", "literal", "exact_match"]` whenever lexical is ready,
    /// `+ ["vector"]` once semantic is ready, `+ ["kg"]` once graph is
    /// ready. Returned in deterministic order so caller diffs stay stable.
    pub fn search_capabilities(&self) -> Vec<&'static str> {
        let mut out = Vec::with_capacity(5);
        if self.lexical.status.is_ready() {
            out.push("bm25");
            out.push("literal");
            out.push("exact_match");
        }
        if self.semantic.status.is_ready() {
            out.push("vector");
        }
        if self.graph.status.is_ready() {
            out.push("kg");
        }
        out
    }

    /// Compute the coarse top-level lifecycle status string used by the
    /// status endpoint's `status` field. Maps to:
    /// `created` → `walking` → `indexed_lexical` → `indexed_vector` → `ready`
    /// (or `ready` directly when stages 2 and 3 are `Skipped`).
    pub fn lifecycle_status(&self) -> &'static str {
        // Issue #601: ANY failed stage dominates the lifecycle status — a
        // zero-vector embed failure must report `failed`, never `ready`, so
        // `/health` and `GET /indexes/:id` surface the dead lane loudly.
        if self.lexical.status == StageStatus::Failed
            || self.semantic.status == StageStatus::Failed
            || self.graph.status == StageStatus::Failed
        {
            return "failed";
        }
        match (self.lexical.status, self.semantic.status, self.graph.status) {
            (StageStatus::Pending, _, _) => "created",
            (StageStatus::InProgress, _, _) => "walking",
            // Lexical ready — categorise by semantic + graph
            (StageStatus::Ready, StageStatus::Skipped, _) => "ready",
            (StageStatus::Ready, StageStatus::Pending, _)
            | (StageStatus::Ready, StageStatus::InProgress, _) => "indexed_lexical",
            (StageStatus::Ready, StageStatus::Ready, StageStatus::Pending)
            | (StageStatus::Ready, StageStatus::Ready, StageStatus::InProgress) => "indexed_vector",
            (StageStatus::Ready, StageStatus::Ready, StageStatus::Ready) => "ready",
            (StageStatus::Ready, StageStatus::Ready, StageStatus::Skipped) => "ready",
            // Skipped lexical is not a state Phase 1 produces, but keep a
            // sensible default so a future opt-in does not crash callers.
            (StageStatus::Skipped, _, _) => "ready",
            // Any `Failed` stage is handled by the early-return guard above, so
            // this is unreachable in practice; the wildcard keeps the match
            // exhaustive and reports `failed` defensively for any residual case.
            _ => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct IndexId(pub String);

impl IndexId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl std::fmt::Display for IndexId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

pub struct IndexHandle {
    pub id: IndexId,
    pub indexer: Arc<RwLock<CodeIndexer>>,
    pub root_path: std::path::PathBuf,

    /// Subtrees (absolute paths) to restrict indexing to. Empty = walk the
    /// entire `root_path`. Sourced from `trusty-search.yaml`'s `paths:` field.
    ///
    /// Why: large polyrepos need to split a single tree into multiple logical
    /// indexes (e.g. `api/` vs `ui/`). Storing the absolute subtree set on the
    /// handle lets the reindex walker prune entire directories without
    /// per-file path arithmetic.
    pub include_paths: Vec<std::path::PathBuf>,

    /// Glob patterns to exclude (on top of the built-in `SKIP_DIRS` /
    /// `should_skip_path` checks). Each pattern is run through
    /// `repo_config::path_matches_any_glob`.
    pub exclude_globs: Vec<String>,

    /// File extension allow-list (without leading dot, e.g. `["rs", "py"]`).
    /// Empty = all supported extensions are indexed.
    pub extensions: Vec<String>,

    /// Domain-specific vocabulary fed to `QueryClassifier::classify_with_domain`
    /// at search time. Empty = standard classifier behaviour.
    pub domain_terms: Vec<String>,

    /// Issue #77 / #118: index prose docs (`*.md`, `CHANGELOG*`, ...).
    /// Default `true` as of v0.8.3 — the per-mode `is_allowed_for_mode`
    /// filter keeps these chunks out of `mode=code` results, and
    /// `mode=text` needs them indexed at all. Set via `include_docs:
    /// false` in `trusty-search.yaml` to opt out.
    pub include_docs: bool,

    /// Issue #100: honour `.gitignore` (plus `.ignore`, `.rgignore`,
    /// `.git/info/exclude`, global gitignore) during the reindex walk.
    /// Default `true`. Set to `false` via `trusty-search.yaml`'s
    /// `respect_gitignore: false` or the `POST /indexes` `respect_gitignore`
    /// field when the operator wants to index a vendored / gitignored
    /// subtree on purpose.
    ///
    /// Why: the historical `walkdir`-based walker ignored gitignore, which
    /// combined with the chunk budget (`TRUSTY_MAX_CHUNKS`) caused silent
    /// partial-index failures — a gitignored subtree full of minified JS
    /// would dominate the budget and the project's real source was never
    /// reached. Surfacing the toggle on the handle lets the reindex walker
    /// thread the operator's choice through to `WalkOptions`.
    /// Test: covered by `service::walker::test_walker_honors_gitignore`
    /// and the persistence-round-trip tests in `service::persistence`.
    pub respect_gitignore: bool,

    /// Glob patterns matched against the *immediate subdirectory name* under
    /// `root_path`. When non-empty, the reindex walker keeps only files
    /// whose first path component (relative to `root_path`) matches one of
    /// these patterns. Issue #111.
    ///
    /// Why: large polyrepo roots (e.g. a directory of cloned Git repos) need
    /// a quick way to scope an index to a subset of sibling repositories
    /// without enumerating every absolute subtree. Glob patterns (`*` only,
    /// no `**`) match repo-name shapes like `common-*` or `duetto-common*`.
    /// What: A `Vec<String>` of glob patterns; empty = scan the whole
    /// `root_path` (current behaviour, no regression).
    /// Test: `path_filter_matches_immediate_subdir` covers the glob logic;
    /// `reindex_honours_path_filter` covers the end-to-end walk.
    pub path_filter: Vec<String>,

    /// Embedded semantic fingerprint of the index's root-level metadata
    /// (`README.md`, `CLAUDE.md`, `Cargo.toml`, …) — issue #112.
    ///
    /// Why: cross-index fan-out (`POST /search`) needs a cheap way to
    /// weight or skip indexes based on query relevance to each project's
    /// description. Storing a single pre-computed embedding here lets the
    /// fan-out handler compute cosine similarity against the query
    /// embedding in O(d) per index, rather than running a full per-index
    /// search probe.
    /// What: `None` when no recognised metadata file was found (cosine
    /// weight defaults to neutral 1.0 in the router); `Some(vec)` carries a
    /// `dim`-length unit-ish vector produced by the same embedder used for
    /// chunks. Populated by [`crate::service::context_inference`] at the
    /// end of every reindex.
    /// Test: `context_embedding_*` tests in
    /// `crate::service::context_inference::tests`.
    pub context_embedding: Arc<RwLock<Option<Vec<f32>>>>,

    /// Truncated (≤500 char) human-readable preview of the metadata
    /// summary that produced [`Self::context_embedding`]. Surfaced via
    /// `GET /indexes/:id/status` for operator visibility.
    pub context_summary: Arc<RwLock<Option<String>>>,

    /// Git HEAD SHA captured at index time (issue #75).
    ///
    /// Why: lets the search response report `results_may_be_stale` by
    /// comparing the indexed SHA against the working tree's current HEAD
    /// SHA. Captured on a best-effort basis at handle registration (and
    /// refreshed by any future reindex hook); `None` outside a git repo or
    /// when `git` is unavailable.
    /// What: an `Arc<RwLock<Option<String>>>` so the daemon can update it
    /// after a successful reindex without rebuilding the handle. Reads are
    /// O(1) and lock-free in practice (read-mostly).
    /// Test: `test_results_may_be_stale_when_head_changes` (server-level
    /// integration coverage).
    pub indexed_head_sha: Arc<RwLock<Option<String>>>,

    /// Staged-pipeline opt-out (issue #109, Phase 1).
    ///
    /// Why: callers who explicitly want a "daemonized ripgrep" without the
    /// embedder cost can set this at create time. The reindex pipeline
    /// returns after Stage 1 and permanently marks the semantic + graph
    /// stages as `Skipped` so the search handler never tries to use the
    /// vector lane on this index.
    /// What: a bare `bool`, set once at `POST /indexes` and persisted to
    /// `indexes.toml` so it survives daemon restarts. Defaults to `false`.
    /// Test: `lexical_only_index_never_runs_stage_2` in
    /// `service::reindex::tests`.
    pub lexical_only: bool,

    /// Stage-1-minimal mode (issue #313): when `true`, the Phase 3 KG
    /// rebuild is skipped entirely during `spawn_reindex_with_cleanup`. The
    /// graph stage is permanently `Skipped` at registration and warm-boot.
    /// `get_call_chain` and `search_kg` return a 503 `kg_unavailable` error.
    ///
    /// Why: for pure BM25/lexical deployments the petgraph DiGraph can
    /// consume 50–100 MB of heap. Setting this flag avoids allocating the
    /// graph at all — not merely gating it at query time. Orthogonal to
    /// `lexical_only`: both flags may be set independently.
    /// What: a bare `bool`, set once at `POST /indexes` and persisted to
    /// `indexes.toml` so it survives daemon restarts. Defaults to `false`.
    /// Test: `skip_kg_index_never_runs_phase3` in `service::reindex::tests`.
    pub skip_kg: bool,

    /// Per-stage lifecycle state surface for the staged pipeline (issue #109).
    ///
    /// Why: search handler reads this to compute `search_capabilities` and
    /// skip lanes whose stage is not yet ready; reindex task flips states
    /// without rebuilding the handle.
    /// What: `IndexStages` with three `StageState` slots (lexical/semantic/
    /// graph). `lexical_only` pre-sets semantic+graph to `Skipped`.
    /// Test: `stage_status_capabilities_*` (registry) and
    /// `service::reindex::tests::stage_*` (e2e).
    pub stages: Arc<RwLock<IndexStages>>,

    /// RFC-3339 timestamp stamped at reindex-complete time (issue #878).
    ///
    /// Why: `index_disk_and_mtime` only checks the legacy global data dir;
    /// colocated and freshly-created indexes return `null`. In-memory stamping
    /// is storage-agnostic and always non-null after a successful reindex.
    /// What: `Arc<RwLock<Option<String>>>` written alongside `indexed_head_sha`
    /// on success. `None` for unindexed / warm-booted handles (status endpoint
    /// falls back to disk mtime). `Some(rfc3339)` after first completed pass.
    /// Test: `last_indexed_stamped_after_reindex` in `service::reindex::tests`.
    pub last_indexed_at: Arc<RwLock<Option<String>>>,

    /// Stage-2 backpressure notifier (issue #109, Phase 1 stub).
    ///
    /// Why: search arrivals ping this to let the embedder briefly yield,
    /// keeping query latency responsive during concurrent reindexes.
    /// What: `Arc<tokio::sync::Notify>` — `notify_one` from handlers.
    /// Test: not directly tested; latency tests cover regression.
    pub search_pressure: Arc<tokio::sync::Notify>,

    /// Walk diagnostic snapshot (issue #280).
    ///
    /// Why: zero-chunk reindexes need per-walk context (errors, file
    /// counts) surfaced in `GET /indexes/:id/status`.
    /// What: `Arc<RwLock<WalkDiagnostics>>` — written at walk start/end,
    /// read by the HTTP handler without holding the reindex lock.
    /// Test: `walk_diagnostics_populated_after_reindex` (reindex tests).
    pub walk_diagnostics: Arc<tokio::sync::RwLock<WalkDiagnostics>>,
}

/// Diagnostic snapshot of the most recent walk (issue #280).
///
/// Why: expose enough information in `GET /indexes/:id/status` for
/// operators to diagnose why a reindex produced zero chunks.
/// What: four fields — timestamp, file counts, and first error — updated
/// atomically at walk start and walk end.  All fields use `Option` and
/// `#[serde(default)]` for backward compatibility when loading old persisted
/// index records.
/// Test: `walk_diagnostics_populated_after_reindex` and
/// `walk_diagnostics_error_captured_on_failure` in `service::reindex::tests`.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct WalkDiagnostics {
    /// RFC-3339 timestamp of when the most recent walk began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_walk_started_at: Option<String>,
    /// Number of files the walker observed (before hash-skip / minified
    /// filtering).
    #[serde(default)]
    pub last_walk_files_seen: u64,
    /// Number of files skipped by the walker (gitignore, binary, oversize,
    /// SKIP_DIRS, etc.).  This is the `skipped_dirs` counter returned by
    /// `walk_source_files_with_options`.
    #[serde(default)]
    pub last_walk_files_skipped: u64,
    /// Human-readable error string if the walk aborted with a top-level
    /// error.  `None` on a clean walk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_walk_error: Option<String>,
}

impl IndexHandle {
    /// Construct a handle with empty filter/domain fields. Convenience for the
    /// many call sites (warm-boot, tests) that don't carry repo-level config.
    ///
    /// `include_docs` defaults to `true` (issue #118) so the bare handle
    /// matches the documented v0.8.3 walk-time behaviour — `mode=text`
    /// works out of the box, and `mode=code` results stay clean via the
    /// post-RRF `is_allowed_for_mode` filter.
    pub fn bare(
        id: IndexId,
        indexer: Arc<RwLock<CodeIndexer>>,
        root_path: std::path::PathBuf,
    ) -> Self {
        Self {
            id,
            indexer,
            root_path,
            include_paths: Vec::new(),
            exclude_globs: Vec::new(),
            extensions: Vec::new(),
            domain_terms: Vec::new(),
            include_docs: true,
            respect_gitignore: true,
            path_filter: Vec::new(),
            context_embedding: Arc::new(RwLock::new(None)),
            context_summary: Arc::new(RwLock::new(None)),
            indexed_head_sha: Arc::new(RwLock::new(None)),
            last_indexed_at: Arc::new(RwLock::new(None)),
            lexical_only: false,
            skip_kg: false,
            stages: Arc::new(RwLock::new(IndexStages::default())),
            search_pressure: Arc::new(tokio::sync::Notify::new()),
            walk_diagnostics: Arc::new(tokio::sync::RwLock::new(WalkDiagnostics::default())),
        }
    }
}

/// Match a file path against a set of glob patterns applied to the immediate
/// subdirectory of `root_path` (issue #111).
///
/// Why: `path_filter` on a registered index lets callers restrict indexing
/// to a subset of immediate subdirectories — usually repo names inside a
/// polyrepo. The match runs against the *first path component* of
/// `path.strip_prefix(root_path)` so the filter never has to know about
/// the deeper file tree.
/// What: returns `true` when `patterns` is empty (no filtering) or when at
/// least one pattern in `patterns` matches the immediate subdir's basename
/// via `glob::Pattern` (`*` wildcards, no `**`). Files that live directly in
/// `root_path` (depth 0) are kept only if a pattern equals `"."` or matches
/// the empty string — typically they aren't there for polyrepo layouts but
/// we err on the side of keeping them when no pattern is supplied.
/// Test: `path_filter_matches_immediate_subdir` in this module.
pub fn path_matches_filter(
    path: &std::path::Path,
    root_path: &std::path::Path,
    patterns: &[String],
) -> bool {
    if patterns.is_empty() {
        return true;
    }
    let Ok(rel) = path.strip_prefix(root_path) else {
        // Path is outside the configured root; treat as non-match so the
        // walker drops it.
        return false;
    };
    let first_component = rel.components().next().and_then(|c| c.as_os_str().to_str());
    let Some(subdir) = first_component else {
        return false;
    };
    for pat in patterns {
        let Ok(pattern) = glob::Pattern::new(pat) else {
            // Malformed pattern: log once via tracing and fall back to a
            // literal string compare so a typo doesn't silently drop every
            // file.
            tracing::warn!("path_filter pattern '{pat}' is not a valid glob; using exact match");
            if pat == subdir {
                return true;
            }
            continue;
        };
        if pattern.matches(subdir) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Verify the glob matcher only inspects the *first* path component
    /// (the immediate subdir name) and supports `*` wildcards.
    #[test]
    fn path_filter_matches_immediate_subdir() {
        let root = PathBuf::from("/data/repos");

        // Empty pattern set ⇒ everything passes.
        assert!(path_matches_filter(
            &PathBuf::from("/data/repos/anything/src/lib.rs"),
            &root,
            &[],
        ));

        // Exact match on immediate subdir.
        let patterns = vec!["duetto-common".to_string()];
        assert!(path_matches_filter(
            &PathBuf::from("/data/repos/duetto-common/src/lib.rs"),
            &root,
            &patterns,
        ));
        assert!(!path_matches_filter(
            &PathBuf::from("/data/repos/other/src/lib.rs"),
            &root,
            &patterns,
        ));

        // Glob wildcard with `*`.
        let patterns = vec!["common-*".to_string(), "duetto-common*".to_string()];
        assert!(path_matches_filter(
            &PathBuf::from("/data/repos/common-utils/foo.rs"),
            &root,
            &patterns,
        ));
        assert!(path_matches_filter(
            &PathBuf::from("/data/repos/duetto-common-events/lib.rs"),
            &root,
            &patterns,
        ));
        assert!(!path_matches_filter(
            &PathBuf::from("/data/repos/totally-other/lib.rs"),
            &root,
            &patterns,
        ));

        // Path outside root → not matched.
        assert!(!path_matches_filter(
            &PathBuf::from("/elsewhere/duetto-common/lib.rs"),
            &root,
            &patterns,
        ));
    }

    /// Multiple matching patterns: any one match is enough.
    #[test]
    fn path_filter_matches_any_pattern() {
        let root = PathBuf::from("/repos");
        let patterns = vec!["api".to_string(), "frontend".to_string()];
        assert!(path_matches_filter(
            &PathBuf::from("/repos/api/handlers.rs"),
            &root,
            &patterns,
        ));
        assert!(path_matches_filter(
            &PathBuf::from("/repos/frontend/app.tsx"),
            &root,
            &patterns,
        ));
        assert!(!path_matches_filter(
            &PathBuf::from("/repos/docs/README.md"),
            &root,
            &patterns,
        ));
    }

    /// Issue #109 Phase 1: `search_capabilities` grows monotonically as
    /// stages advance from `Pending` → `Ready`. Mirrors the wire contract
    /// the search handler relies on for graceful degradation.
    #[test]
    fn stage_status_capabilities_grow_with_stages() {
        let mut stages = IndexStages::default();
        // All pending → no capabilities.
        assert!(stages.search_capabilities().is_empty());
        assert_eq!(stages.lifecycle_status(), "created");

        // Lexical ready → bm25 + literal + exact_match.
        stages.lexical.status = StageStatus::Ready;
        let caps = stages.search_capabilities();
        assert_eq!(caps, vec!["bm25", "literal", "exact_match"]);
        assert_eq!(stages.lifecycle_status(), "indexed_lexical");

        // + semantic ready → adds vector.
        stages.semantic.status = StageStatus::Ready;
        let caps = stages.search_capabilities();
        assert!(caps.contains(&"vector"));
        assert!(!caps.contains(&"kg"));
        assert_eq!(stages.lifecycle_status(), "indexed_vector");

        // + graph ready → adds kg, top-level ready.
        stages.graph.status = StageStatus::Ready;
        let caps = stages.search_capabilities();
        assert!(caps.contains(&"kg"));
        assert_eq!(stages.lifecycle_status(), "ready");
    }

    /// `lexical_only` indexes pre-mark semantic + graph as `Skipped`. The
    /// lifecycle status must report `ready` once stage 1 finishes, NOT
    /// `indexed_lexical` (which would imply more stages are coming).
    #[test]
    fn stage_status_lexical_only_treats_skipped_as_terminal() {
        let stages = IndexStages {
            lexical: StageState {
                status: StageStatus::Ready,
                ..Default::default()
            },
            semantic: StageState::skipped(),
            graph: StageState::skipped(),
        };
        // Lexical-only index: search_capabilities should be lexical-only.
        let caps = stages.search_capabilities();
        assert_eq!(caps, vec!["bm25", "literal", "exact_match"]);
        assert!(!caps.contains(&"vector"));
        assert!(!caps.contains(&"kg"));
        // And the top-level status reflects terminal completion.
        assert_eq!(stages.lifecycle_status(), "ready");
    }

    /// In-progress lexical → top-level status is `walking`. The search
    /// handler treats this as "no capabilities available yet" and falls
    /// back to grep.
    #[test]
    fn stage_status_walking_during_stage_1() {
        let mut stages = IndexStages::default();
        stages.lexical.status = StageStatus::InProgress;
        assert_eq!(stages.lifecycle_status(), "walking");
        assert!(stages.search_capabilities().is_empty());
    }

    /// Malformed glob pattern: falls back to literal match so a typo never
    /// silently drops every file. The non-matching path still returns false.
    #[test]
    fn path_filter_malformed_pattern_falls_back_to_exact() {
        let root = PathBuf::from("/r");
        // `[` opens a glob character class that is never closed.
        let patterns = vec!["[unclosed".to_string()];
        assert!(path_matches_filter(
            &PathBuf::from("/r/[unclosed/file.rs"),
            &root,
            &patterns,
        ));
        assert!(!path_matches_filter(
            &PathBuf::from("/r/other/file.rs"),
            &root,
            &patterns,
        ));
    }
}

/// Machine-wide index registry. DashMap = concurrent, shard-locked.
/// Multiple axum handlers can read different indexes simultaneously.
#[derive(Default, Clone)]
pub struct IndexRegistry {
    indexes: Arc<DashMap<IndexId, Arc<IndexHandle>>>,
}

impl IndexRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&self, handle: IndexHandle) -> Arc<IndexHandle> {
        let handle = Arc::new(handle);
        self.indexes.insert(handle.id.clone(), Arc::clone(&handle));
        handle
    }

    pub fn get(&self, id: &IndexId) -> Option<Arc<IndexHandle>> {
        self.indexes.get(id).map(|r| Arc::clone(&*r))
    }

    pub fn list(&self) -> Vec<IndexId> {
        self.indexes.iter().map(|r| r.key().clone()).collect()
    }

    /// Return all registered handles as `Arc<IndexHandle>` values.
    ///
    /// Why: `GET /indexes?format=tree` needs the `root_path` of every handle to
    /// build the hierarchy entries; iterating by ID + `get()` would require
    /// a separate lock acquisition per entry.  A single shard-iteration pass is
    /// cheaper and avoids re-cloning the ID vector.
    /// What: iterates the DashMap once, cloning each `Arc<IndexHandle>`.
    /// Test: implied by `list_indexes_tree_format_shape` server tests.
    pub fn list_handles(&self) -> Vec<Arc<IndexHandle>> {
        self.indexes.iter().map(|r| Arc::clone(&*r)).collect()
    }

    /// Drop an index from the registry. Returns true if the entry existed.
    ///
    /// Why: `DELETE /indexes/:id` (admin UI) needs a way to evict an index
    /// without restarting the daemon.
    /// What: shard-locked remove via DashMap; the previous `Arc<IndexHandle>`
    /// is dropped when the last reader finishes (RwLock readers from in-flight
    /// search requests keep it alive briefly, which is safe).
    /// Test: register → unregister → get returns None.
    pub fn unregister(&self, id: &IndexId) -> bool {
        self.indexes.remove(id).is_some()
    }

    pub fn len(&self) -> usize {
        self.indexes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.indexes.is_empty()
    }
}
