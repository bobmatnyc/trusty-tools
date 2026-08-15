//! Persistence helpers: registry TOML + per-index data directories (issue #85).
//!
//! Why: centralises filesystem layout and (de)serialization so startup, DELETE,
//! and warm-boot all agree on paths. Covers: registry TOML (`indexes_toml_path`
//! / `load_index_registry` / `save_index_registry`), per-index data dirs
//! (`index_data_dir`), and on-disk deletion (`remove_index_data_dir`). LRU
//! timestamp helpers (issue #993) are in `persistence_timestamps.rs` and
//! re-exported here.
//! Test: `tests::registry_roundtrip` and `persistence_timestamps::tests`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// On-disk record for one registered index. Kept tiny so the TOML file stays
/// human-readable for ops debugging.
///
/// Why `#[non_exhaustive]`: this struct grows a field every time the daemon
/// learns to remember one more thing across a restart, and each addition used
/// to be a semver-major break for any outside caller building it with a struct
/// literal — #4088 shipped exactly that break on a patch bump and cost a
/// downstream crate a yank. `#[non_exhaustive]` makes future field additions
/// non-breaking by construction, which is what this repo's release convention
/// asks for. Adding the attribute is ITSELF breaking, so it lands here, in the
/// release that was already taking a minor bump for the #4390 / #4391 fields.
/// What: outside this crate, construct with [`PersistedIndex::new`] (or
/// `Default::default()`) and assign the fields you need — every field stays
/// `pub`, so only the literal SYNTAX is withdrawn, not write access. In-crate
/// construction is unaffected.
/// Test: `service::boot_markers_tests::legacy_entries_load_with_both_markers_absent`
/// pins the deserialisation contract; the `tests/` integration suites exercise
/// the constructor.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct PersistedIndex {
    pub id: String,
    pub root_path: PathBuf,
    /// Subtrees (relative to `root_path`) to restrict indexing to. Sourced
    /// from `trusty-search.yaml`'s `paths:` field. `#[serde(default)]` so
    /// older `indexes.toml` files without these fields keep loading.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_paths: Vec<String>,
    /// Glob patterns to exclude on top of the built-in ignores.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_globs: Vec<String>,
    /// Extension allow-list (e.g. `["rs", "py"]`, without leading dot).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    /// Domain vocabulary for the per-index intent classifier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub domain_terms: Vec<String>,
    /// Glob patterns matched against immediate subdirectory names under
    /// `root_path`. When non-empty, only files inside subdirectories whose
    /// basename matches at least one pattern are indexed. Distinct from
    /// `include_paths` (which holds absolute subtrees from
    /// `trusty-search.yaml`) — `path_filter` is the API-level glob filter
    /// added for issue #111, intended for filtering polyrepo monorepos by
    /// repo-name pattern.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_filter: Vec<String>,

    /// Issue #77 / #118: index prose docs (`*.md`, `CHANGELOG*`, …).
    /// Default `true` as of v0.8.3 (issue #118) — code-mode results stay
    /// clean via the per-mode `is_allowed_for_mode` filter, and text-mode
    /// searches need the docs to be indexed at all. Persisted so per-index
    /// opt-outs (`include_docs = false` in `trusty-search.yaml`) survive
    /// daemon restarts. The serde default deserialises missing fields as
    /// `true`, so older `indexes.toml` entries written under v0.8.2 (where
    /// the field was omitted because it matched the then-default `false`)
    /// will now load as `true` on first read — the migration the ticket
    /// calls out. Indexes that explicitly persisted `include_docs = false`
    /// keep their opt-out.
    #[serde(default = "default_include_docs", skip_serializing_if = "is_true")]
    pub include_docs: bool,

    /// Issue #100: honour `.gitignore` (plus `.ignore`, `.rgignore`,
    /// `.git/info/exclude`, global gitignore) during the reindex walk.
    /// Default `true` — matches ripgrep semantics. Older `indexes.toml`
    /// files predate this field; the serde default deserialises them as
    /// `true` so the fix takes effect on restart without rewriting state.
    /// `skip_serializing_if` keeps the TOML compact: only the rare
    /// opt-out (`respect_gitignore = false`) is written to disk.
    #[serde(
        default = "default_respect_gitignore",
        skip_serializing_if = "is_default_respect_gitignore"
    )]
    pub respect_gitignore: bool,

    /// Whether the reindex walker dereferences symlinks during traversal.
    ///
    /// Why: roots containing symlinks that escape the tree (self-referential
    /// links, links into unrelated repos) bloat / corrupt the index when
    /// followed. Newly created indexes opt out (`follow_links = false` at
    /// `POST /indexes`); the value is persisted so `reindex` honours it.
    ///
    /// Backward-compat decision: the serde default is **`true`** (see
    /// [`default_follow_links`]). Existing `indexes.toml` entries predate this
    /// field and were built while the walker followed symlinks
    /// unconditionally; deserialising a missing field as `true` keeps those
    /// indexes behaving exactly as before on upgrade — the least-surprising
    /// choice. New indexes get `false` from the create handler, not from this
    /// default. `skip_serializing_if` keeps the TOML compact: only the value
    /// that differs from the serde default (`false`, the new-index opt-out) is
    /// written to disk.
    #[serde(
        default = "default_follow_links",
        skip_serializing_if = "is_default_follow_links"
    )]
    pub follow_links: bool,

    /// Issue #1372: extra directory basenames pruned during the reindex walk on
    /// top of the built-in `walker::SKIP_DIRS`. Sourced from
    /// `trusty-search.yaml`'s `extra_skip_dirs:` field, the `POST /indexes`
    /// field, or `PATCH /indexes/:id/config`.
    ///
    /// Why: persisted so a data-export-pruning choice survives daemon restarts.
    /// What: `#[serde(default = "default_extra_skip_dirs")]` so legacy
    /// `indexes.toml` files (written before this field existed) load with the
    /// targeted default set (`data`/`exports`/`output`/`reports`/`snapshots`/
    /// `results`) rather than an empty list — i.e. the hygiene fix takes effect
    /// on the next warm boot. The default is always written to disk (no
    /// `skip_serializing_if`) so the value is discoverable and editable.
    /// Test: `data_file_hygiene_round_trips` in this module.
    #[serde(default = "default_extra_skip_dirs")]
    pub extra_skip_dirs: Vec<String>,

    /// Issue #1372: tighter size cap (bytes) applied only to data-ish file
    /// extensions (`walker::DATA_EXTS`: json/xml/txt/log). `None` ⇒ use the
    /// built-in default (`walker::DEFAULT_DATA_FILE_MAX_BYTES`, 64 KiB).
    ///
    /// Why: persisted so the data-file cap survives daemon restarts.
    /// What: `#[serde(default = "default_data_file_max_bytes")]` so legacy
    /// `indexes.toml` files load with `Some(65536)` and pick up the cap on the
    /// next warm boot. Always serialised (no `skip_serializing_if`) so the value
    /// is discoverable and editable.
    /// Test: `data_file_hygiene_round_trips` in this module.
    #[serde(default = "default_data_file_max_bytes")]
    pub data_file_max_bytes: Option<u64>,

    /// Staged-pipeline opt-out (issue #109, Phase 1): when `true`, the
    /// reindex pipeline stops after Stage 1 (lexical / BM25 / redb) and
    /// never embeds. Useful for callers who explicitly want a daemonized
    /// ripgrep without the embedder overhead.
    ///
    /// Why: persisted so an `indexes.toml` round-trip preserves the
    /// caller's choice across daemon restarts; otherwise the next warm
    /// boot would silently re-enable the embedder lane and the operator's
    /// disk + CPU savings would evaporate.
    /// What: `#[serde(default)]` so older `indexes.toml` files load as
    /// `false` (full pipeline), and `skip_serializing_if = "std::ops::Not::not"`
    /// keeps the TOML compact — only `true` is written to disk.
    /// Test: `lexical_only_round_trips` in this module.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lexical_only: bool,

    /// Stage-1-minimal mode (issue #313): when `true`, the KG rebuild
    /// (Phase 3 of `spawn_reindex_with_cleanup`) is skipped entirely.
    /// The graph stage is permanently `Skipped` at warm-boot and after
    /// every reindex. `get_call_chain` and `search_kg` return a
    /// `503 kg_unavailable` error rather than an empty result.
    ///
    /// Why: for pure BM25 / lexical deployments the petgraph DiGraph can
    /// consume 50–100 MB of heap for a large corpus. Setting this flag
    /// avoids building the graph at all, not just gating it at query time.
    /// Orthogonal to `lexical_only` — both flags may be set independently.
    /// What: `#[serde(default)]` so existing `indexes.toml` files load as
    /// `false`; only `true` is written to disk to keep the file compact.
    /// Test: `skip_kg_round_trips` in this module.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skip_kg: bool,

    /// Vector/semantic-suppression flag (issue #2984 Phase 1): when `true`,
    /// the embedder is never invoked — no HNSW work, ever. The semantic
    /// stage is permanently `Skipped` at warm-boot and after every reindex.
    /// Orthogonal to `skip_kg`: `skip_kg=false, skip_vector=true` is the
    /// "KG-on, vector-off" quadrant.
    ///
    /// Why: mirrors `skip_kg`'s persistence contract so an `indexes.toml`
    /// round-trip preserves the caller's per-component choice across daemon
    /// restarts — both create-time (`POST /indexes`) and runtime
    /// (`PATCH /indexes/:id/config`).
    /// What: `#[serde(default)]` so older `indexes.toml` files load as
    /// `false` (vector enabled); only `true` is written to disk.
    /// Test: `skip_vector_round_trips` in this module.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skip_vector: bool,

    /// Deferred-embedding mode (issue #923): when `true` (the default), the
    /// fast pass (walk → chunk → BM25 → KG) runs synchronously and marks
    /// lexical + graph stages `Ready` in seconds. Embedding is deferred to a
    /// background job. Set to `false` to force the old synchronous full index
    /// (semantic `Ready` before the call returns). Has no effect when
    /// `lexical_only = true`.
    ///
    /// Why: persisted so an `indexes.toml` round-trip preserves the caller's
    /// choice — `false` survives daemon restarts without the operator
    /// re-specifying the opt-out.
    /// What: defaults to `true` (deferred is the new default). Only `false`
    /// is written to disk (via `skip_serializing_if = "is_true"`) so existing
    /// `indexes.toml` files continue to load as `true`.
    /// Test: `defer_embed_round_trips` in this module.
    // Serde asymmetry note: `lexical_only` / `skip_kg` use `Not::not` to skip
    // the default `false`; `defer_embed` uses `is_true` to skip the default
    // `true`. The helper name reflects the value that equals the default.
    #[serde(default = "default_defer_embed", skip_serializing_if = "is_true")]
    pub defer_embed: bool,

    /// Issue #403: whether this index uses colocated storage (`<root_path>/.trusty-search/`)
    /// rather than the legacy global data directory (`<data_dir>/indexes/<id>/`).
    ///
    /// Why: colocated storage keeps index data inside the project tree so two
    /// git worktrees at different filesystem paths have independent indexes, and
    /// moving a project tree does not invalidate its index. This flag is set by
    /// `trusty-search index` (new indexes) and by `trusty-search migrate storage`
    /// (migrated legacy indexes). Older `indexes.toml` files never set this field
    /// so they load as `false` (legacy global storage) — no back-compat breakage.
    /// What: when `true`, all persistence path helpers (`hnsw_path`,
    /// `corpus_redb_path`, `schema_version_path`, `corpus_redb_tmp_path`) route
    /// to `<root_path>/.trusty-search/` instead of `<data_dir>/indexes/<id>/`.
    /// `#[serde(default)]` ensures the field is absent in TOML for false (compact).
    /// Test: `colocated_flag_round_trips` in this module.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub colocated: bool,

    /// Unix timestamp of the most recent query against this index (issue #993).
    ///
    /// Why: `TRUSTY_WARMBOOT_MAX_INDEXES` ranks indexes by recency to decide which
    /// to warm-boot eagerly. Sort key = `max(last_queried_unix, last_indexed_unix)`.
    /// `None` means never queried (first-run / pre-upgrade).
    /// What: updated fire-and-forget in `search_handler` (rate-limited ≤ 60 s).
    /// Test: `persistence_timestamps::tests::last_queried_and_indexed_round_trips`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_queried_unix: Option<u64>,

    /// Unix timestamp of the most recent completed reindex of this index (issue #993).
    ///
    /// Why: companion to `last_queried_unix` for the lazy warm-boot LRU key so
    /// recently-reindexed-but-rarely-queried indexes (CI agents) stay in the eager set.
    /// What: written at reindex SSE `complete`. `None` until first reindex.
    /// Test: `persistence_timestamps::tests::last_queried_and_indexed_round_trips`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_indexed_unix: Option<u64>,

    /// Canonical repository identity (DOC-37, issue #2611) — the path-independent
    /// join key relating this index to the other facets (live checkout, `.base`
    /// clone, session worktrees) of the SAME repo.
    ///
    /// Why: `id` is a bare path basename, so a repo's live checkout, its
    /// tm-managed `.base` clone, and each session worktree register as unrelated
    /// indexes. Storing the canonical
    /// [`trusty_common::repo_identity::RepoIdentity`] string (`owner/repo`, or
    /// `content:<sha>` for remoteless repos) alongside `id` lets an operator
    /// group and filter every index of one repo — including orphaned worktree
    /// entries whose `root_path` no longer exists, where re-deriving from disk is
    /// impossible.
    /// What: `Option<String>` holding `RepoIdentity::canonical()`. `None` for
    /// pre-DOC-37 indexes and for roots with no derivable identity; computed at
    /// registration and backfilled at warm-boot where derivable, so existing
    /// `indexes.toml` files load unchanged. `skip_serializing_if` keeps the TOML
    /// compact — only a resolved identity is written.
    /// Test: `repo_identity_round_trips` in `persistence_tests.rs`; grouping is
    /// covered by `prune_orphans::tests`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_identity: Option<String>,

    /// #4095: unix timestamp when this entry's root was FIRST found missing
    /// with multiple ambiguous relocation candidates; `None` while it resolves
    /// cleanly.
    ///
    /// Why: the relocation scan defers on ambiguity and, before this field,
    /// never revisited — so a deferral was permanent registry debris that kept
    /// `search_health` reporting `degraded` forever. A live daemon carried 11
    /// such entries deferred continuously since 2026-06-03. The deferral clock
    /// must survive daemon restarts (the incident spanned many), so it lives in
    /// `indexes.toml` rather than in memory.
    /// What: stamped on the first ambiguous observation and cleared whenever
    /// the entry resolves again; the reaper compares it against
    /// `orphan_reaper::ambiguous_root_grace_secs()` to decide when to drop the
    /// *registration* (never the on-disk data).
    /// Test: `classify_ambiguous_root_*` in `service::orphan_reaper`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ambiguous_root_since_unix: Option<u64>,

    /// #4391: the git HEAD SHA this index's corpus was last built against.
    ///
    /// Why: `IndexHandle::indexed_head_sha` existed only in memory, and both
    /// restore paths re-derived it from LIVE git — so boot reconcile compared
    /// current HEAD against current HEAD and reported `up_to_date` for every
    /// git-backed index, on every boot. Commits landing while the daemon was
    /// down were structurally undetectable. Persisting the stamp is what makes
    /// `reconcile_git_path` able to see pre-restore drift at all.
    /// What: written wherever the in-memory stamp is written — `finish_reindex`
    /// after a completed reindex and `reconcile::stamp_handle` after a delta.
    /// `#[serde(default)]` so legacy `indexes.toml` entries load as `None`;
    /// restore then BACKFILLS the live SHA once (see
    /// `boot_markers::resolve_indexed_head_sha`), so the first boot after
    /// upgrade behaves exactly as before and every later boot has a genuine
    /// stored value.
    /// Test: `resolve_prefers_the_persisted_head_sha_over_live_git` in
    /// `boot_markers`'s `tests` submodule.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexed_head_sha: Option<String>,

    /// #4390: `true` between the moment a deferred-embed (C2) pass is queued
    /// and the moment it commits.
    ///
    /// Why: C2 builds every embedding in memory and commits once at the end, so
    /// a daemon stop anywhere inside the pass persists ZERO new vectors — and
    /// nothing recorded that. Warm boot infers `semantic: ready` from the mere
    /// existence of an HNSW snapshot file, so the stranded chunks (the most
    /// recently edited code) were silently missing from the vector lane until
    /// some unrelated reindex happened to re-queue the pass.
    /// What: set before the pass is enqueued and cleared when it publishes a
    /// terminal state; restore re-arms the catch-up when it loads as `true`.
    /// The asymmetry is deliberate — a lost SET costs only the detection this
    /// field adds, while a lost CLEAR costs one idempotent gap-filling pass, so
    /// every write failure errs toward re-running rather than toward silence.
    /// `#[serde(default)]` + `skip_serializing_if` keep legacy files loading as
    /// `false` and the TOML compact.
    /// Test: `restore_rearms_an_interrupted_deferred_embed_pass` in
    /// `commands::start_restore`'s `markers_tests` submodule.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deferred_embed_pending: bool,
}

/// Why: serde's `default` attribute needs a free function (closures aren't
/// allowed). Centralising the default here keeps it identical for
/// deserialisation and for the `PersistedIndex::default()` fallback.
fn default_respect_gitignore() -> bool {
    true
}

/// Default for `follow_links` when the field is ABSENT from `indexes.toml`
/// (i.e. an index that predates the field): `true`. Those indexes were built
/// while the walker followed symlinks unconditionally, so deserialising a
/// missing field as `true` preserves their exact behaviour on upgrade. Newly
/// created indexes receive `false` from the `POST /indexes` handler instead —
/// this default only governs the legacy-migration path.
fn default_follow_links() -> bool {
    true
}

/// Why: skip writing `follow_links = true` to TOML (it equals the serde
/// default) so existing `indexes.toml` files stay compact and the only value
/// ever persisted is the new-index opt-out (`follow_links = false`).
fn is_default_follow_links(v: &bool) -> bool {
    *v
}

/// Default for `defer_embed`: `true` — the fast-pass / deferred-embed mode is
/// the default behaviour for all new and loaded indexes (issue #923).
fn default_defer_embed() -> bool {
    true
}

/// Default for `extra_skip_dirs` (issue #1372): the targeted data-export
/// directory set. Centralised so the serde missing-field default and the manual
/// `Default` impl agree, and so legacy `indexes.toml` files pick up the fix.
fn default_extra_skip_dirs() -> Vec<String> {
    crate::service::walker::default_extra_skip_dirs()
}

/// Default for `data_file_max_bytes` (issue #1372): `Some(64 KiB)`. A `None`
/// stored value is interpreted by the loader as "use the built-in default".
fn default_data_file_max_bytes() -> Option<u64> {
    Some(crate::service::walker::DEFAULT_DATA_FILE_MAX_BYTES)
}

/// Resolve a persisted `data_file_max_bytes` (`Option<u64>`) to the concrete
/// cap the walker expects, falling back to
/// [`walker::DEFAULT_DATA_FILE_MAX_BYTES`](crate::service::walker::DEFAULT_DATA_FILE_MAX_BYTES)
/// when the stored value is `None` (issue #1372).
pub fn resolve_data_file_max_bytes(stored: Option<u64>) -> u64 {
    stored.unwrap_or(crate::service::walker::DEFAULT_DATA_FILE_MAX_BYTES)
}

/// Why (issue #118): `include_docs` flipped from `false` → `true` in v0.8.3
/// so `text` mode returns useful results out of the box. Centralised so the
/// serde missing-field default and the manual `Default` impl agree.
fn default_include_docs() -> bool {
    true
}

/// Why: skip writing `true` to TOML when the field equals its default —
/// only the rare opt-out (`include_docs = false`, `respect_gitignore =
/// false`) is persisted. Shared by both `include_docs` and
/// `respect_gitignore` since they're both now `true`-by-default booleans.
fn is_true(v: &bool) -> bool {
    *v
}

/// Why: skip writing `respect_gitignore = true` to TOML (it's the default)
/// so existing `indexes.toml` files stay compact and we don't churn every
/// existing index file on the first save.
fn is_default_respect_gitignore(v: &bool) -> bool {
    *v
}

impl Default for PersistedIndex {
    fn default() -> Self {
        Self {
            id: String::new(),
            root_path: PathBuf::new(),
            include_paths: Vec::new(),
            exclude_globs: Vec::new(),
            extensions: Vec::new(),
            domain_terms: Vec::new(),
            path_filter: Vec::new(),
            include_docs: true,
            respect_gitignore: true,
            follow_links: default_follow_links(),
            extra_skip_dirs: default_extra_skip_dirs(),
            data_file_max_bytes: default_data_file_max_bytes(),
            lexical_only: false,
            skip_kg: false,
            skip_vector: false,
            defer_embed: true,
            colocated: false,
            last_queried_unix: None,
            last_indexed_unix: None,
            repo_identity: None,
            ambiguous_root_since_unix: None,
            indexed_head_sha: None,
            deferred_embed_pending: false,
        }
    }
}

impl PersistedIndex {
    /// Build an entry for `id` rooted at `root_path`, everything else default.
    ///
    /// Why: `#[non_exhaustive]` withdraws struct-literal syntax outside this
    /// crate, and `id` / `root_path` are the two fields with no meaningful
    /// default — an entry without them addresses nothing. This is the
    /// replacement for `PersistedIndex { id, root_path, ..Default::default() }`.
    /// What: `Default::default()` with the two identity fields filled in. Every
    /// other field stays `pub`, so a caller assigns what it needs afterwards.
    /// Test: used throughout the `tests/` integration suites.
    pub fn new(id: impl Into<String>, root_path: impl Into<PathBuf>) -> Self {
        Self {
            id: id.into(),
            root_path: root_path.into(),
            ..Default::default()
        }
    }
}

// Issue #993: LRU timestamp helpers live in `persistence_timestamps`; re-exported here.
pub use super::persistence_timestamps::{
    read_last_queried_unix, update_last_indexed_unix, update_last_queried_unix, warmboot_sort_key,
};

/// TOML wrapper so the file uses `[[index]]` array-of-tables syntax —
/// matches the public format documented in CLAUDE.md.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct IndexRegistryFile {
    #[serde(default, rename = "index")]
    pub indexes: Vec<PersistedIndex>,
}

/// Resolve the daemon's data directory (absolute, cwd-independent).
///
/// Why: `daemon_dir` lives behind `DaemonError`. Issue #281: `TRUSTY_DATA_DIR`
/// lets isolated daemons coexist with the production daemon.
/// Issue #718: three-level fallback for launchd's posix_spawn context where
/// `dirs::data_local_dir()` (NSFileManager-backed) can return None on macOS 26:
/// (1) `TRUSTY_DATA_DIR` env var, (2) `dirs::data_local_dir()`,
/// (3) `$HOME`-relative path via `service::data_dir::data_dir_home_fallback`.
/// Issues #4094 / #4255: in a TEST PROCESS the un-overridden fallback
/// resolves to an isolated per-process directory instead of the operator's
/// live data dir, so a test that never sets `TRUSTY_DATA_DIR` cannot register
/// indexes in the real `indexes.toml` (see [`default_data_dir`]).
/// What: returns an absolute path, creating the directory if missing.
/// Test: `data_dir_respects_trusty_data_dir_env_var`, `data_dir_override_yields_absolute_path`,
/// `data_dir_home_fallback_path_is_absolute`,
/// `test_harness_data_dir_is_isolated_from_real_user_data_dir`,
/// `production_data_dir_is_the_real_user_location`.
pub fn data_dir() -> Result<PathBuf> {
    if let Ok(override_dir) = std::env::var("TRUSTY_DATA_DIR") {
        let dir = PathBuf::from(&override_dir);
        anyhow::ensure!(
            dir.is_absolute(),
            "TRUSTY_DATA_DIR must be an absolute path (got: {})",
            override_dir
        );
        std::fs::create_dir_all(&dir).context("create TRUSTY_DATA_DIR data dir")?;
        tracing::debug!("data_dir: TRUSTY_DATA_DIR override: {}", dir.display());
        return Ok(dir);
    }
    // #4094: the fallback is cfg-split so test builds can never reach the
    // developer's live data dir and register fixture indexes there.
    default_data_dir()
}

/// The un-overridden data-dir fallback: the real, well-known user data
/// location.
///
/// Why: split out of [`data_dir`] so the test build can substitute an
/// isolated location without any `cfg!` branching inside the hot resolver
/// (issue #4094).
/// What: `dirs::data_local_dir()/trusty-search`, or the `$HOME`-relative
/// fallback when `NSFileManager` is unavailable (issue #718: launchd
/// posix_spawn, macOS 26).
/// Test: exercised by every production caller; not reachable from ANY test
/// process by construction (see the test-harness branch below).
pub(super) fn default_data_dir() -> Result<PathBuf> {
    // #4255: a test process gets an isolated dir instead of the operator's.
    if trusty_common::running_under_test_harness() {
        return super::data_dir::test_harness_data_dir();
    }
    production_data_dir()
}

/// The real, operator-facing data location — reached only when this process
/// is not a test binary.
///
/// Why: split from [`default_data_dir`] so the test-harness branch above is a
/// single guarded call rather than a `cfg` fork of the whole resolver, and so
/// the production path stays independently readable.
/// What: `dirs::data_local_dir()/trusty-search`, falling back to the
/// `$HOME`-relative path when `NSFileManager` is unavailable (issue #718).
/// Test: `production_data_dir_is_the_real_user_location` pins the location;
/// `test_harness_data_dir_is_isolated_from_real_user_data_dir` proves tests
/// never reach it.
fn production_data_dir() -> Result<PathBuf> {
    if let Some(base) = dirs::data_local_dir() {
        let dir = base.join("trusty-search");
        std::fs::create_dir_all(&dir).context("create trusty-search data dir")?;
        tracing::debug!("data_dir: dirs::data_local_dir: {}", dir.display());
        return Ok(dir);
    }
    // Issue #718: NSFileManager unavailable (launchd posix_spawn, macOS 26).
    super::data_dir::data_dir_home_fallback()
}

/// Path to the registry TOML file.
pub fn indexes_toml_path() -> Result<PathBuf> {
    Ok(data_dir()?.join("indexes.toml"))
}

/// Per-index data directory. Creates `<data_dir>/indexes/<id>/` if missing.
///
/// Why: each index has its own subdir for its HNSW snapshot and chunks file.
/// Centralising the layout here means `commit_parsed_batch`, the daemon's
/// shutdown handler, and `delete_index_handler` all agree on the same paths.
/// What: returns `<data_dir>/indexes/<id>/` after creating the parent tree.
/// Test: `tests::per_index_dir_created` checks the dir exists after the call.
pub fn index_data_dir(index_id: &str) -> Result<PathBuf> {
    let dir = data_dir()?.join("indexes").join(sanitize_id(index_id));
    std::fs::create_dir_all(&dir).context("create per-index data dir")?;
    Ok(dir)
}

/// Public wrapper exposing [`sanitize_id`] for callers — including the binary's
/// command handlers — that need to derive the same on-disk path as
/// [`index_data_dir`] without triggering its `create_dir_all` side effect.
///
/// Why: the binary's `commands/` modules cannot access `pub(crate)` items from
///      the library; making this `pub` lets `prune::default_size_fn` compute the
///      canonical index data-dir path without calling `index_data_dir`.
/// What: returns `sanitize_id(id)` — the exact same path component used by
///       `index_data_dir`, `remove_index_data_dir`, and `hnsw_path`.
/// Test: covered transitively by `prune_tests::prune_*` tests.
pub fn sanitize_id_for_path(id: &str) -> String {
    sanitize_id(id)
}

/// Sanitize an index id for use as a filesystem path component. Replaces any
/// character that isn't `[A-Za-z0-9._-]` with `_` so a user-supplied id can't
/// escape the parent directory or trigger Windows reserved-name issues.
fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Path to the HNSW snapshot file for a given index.
pub fn hnsw_path(index_id: &str) -> Result<PathBuf> {
    Ok(index_data_dir(index_id)?.join("hnsw.usearch"))
}

/// Path to the legacy JSON chunk corpus snapshot for a given index.
///
/// Retained for the issue #28 migration path: a daemon upgraded from a
/// JSON-snapshot build reads this once to seed the redb corpus, after which
/// [`corpus_redb_path`] is authoritative.
pub fn chunks_path(index_id: &str) -> Result<PathBuf> {
    Ok(index_data_dir(index_id)?.join("chunks.json"))
}

/// Path to the redb-backed durable chunk corpus for a given index (issue #28).
///
/// Why: redb replaces the full-rewrite `chunks.json` snapshot with a
/// transactional KV store written incrementally per batch. Each index gets one
/// `index.redb` file under its data dir.
/// What: returns `<data_dir>/indexes/<id>/index.redb`.
/// Test: covered indirectly by the corpus roundtrip integration test.
pub fn corpus_redb_path(index_id: &str) -> Result<PathBuf> {
    Ok(index_data_dir(index_id)?.join("index.redb"))
}

/// Path to the per-index schema-version stamp file (issue #179).
///
/// Why: the `trusty-common::migrations` runner persists the applied
/// `SchemaVersion` next to the index data so warm-boot can decide whether
/// the JSON → redb migration has already run. Centralising the layout here
/// keeps the stamp adjacent to `index.redb` / `chunks.json` and ensures the
/// persistence loader and the migration registry agree on one path.
/// What: returns `<data_dir>/indexes/<id>/schema_version.json`.
/// Test: covered indirectly by `persistence_loader` integration tests; the
/// file-stamp round-trip itself is unit-tested in
/// `trusty_common::migrations::file_stamp`.
pub fn schema_version_path(index_id: &str) -> Result<PathBuf> {
    Ok(index_data_dir(index_id)?.join("schema_version.json"))
}

/// Path to the staging redb corpus written during a `--force` reindex
/// (issue #28, Phase 4).
///
/// Why: a `--force` reindex rebuilds the entire corpus. Writing those chunks
/// directly into the live `index.redb` would expose a partially-rebuilt corpus
/// to concurrent searches (and to a crash mid-reindex). Phase 4 stages the new
/// corpus in a sibling `index.redb.tmp` file and atomically renames it over
/// `index.redb` only once the reindex has fully completed.
/// What: returns `<data_dir>/indexes/<id>/index.redb.tmp`.
/// Test: covered by `tests::test_force_reindex_atomic_corpus_swap`.
pub fn corpus_redb_tmp_path(index_id: &str) -> Result<PathBuf> {
    Ok(index_data_dir(index_id)?.join("index.redb.tmp"))
}

/// Path to the staging HNSW snapshot written by the periodic incremental
/// persister while a reindex is `Running` (issue #3970).
///
/// Why: mirrors [`corpus_redb_tmp_path`] — every periodic HNSW checkpoint
/// during a reindex writes here instead of the live [`hnsw_path`], so the
/// live snapshot is never overwritten with partial state. Fixed filename
/// (not per-run-unique), matching the redb corpus tmp path's convention: the
/// NEXT reindex attempt's periodic saves simply overwrite this file again, so
/// staging never accumulates across retries. If a reindex is interrupted and
/// never retried, this file is left on disk until the next reindex attempt —
/// the same accepted trade-off [`corpus_redb_tmp_path`] already has (no
/// boot-time sweep exists for either).
/// What: returns `<data_dir>/indexes/<id>/hnsw.reindex-staging.usearch`. The
/// filename is deliberately ordered so `Path::with_extension("keys.json")`
/// (the same idiom `UsearchStore::save` uses to derive a sidecar from its
/// binary path) yields the matching staging sidecar
/// `hnsw.reindex-staging.keys.json` — putting `.usearch` last, as the live
/// `hnsw.usearch` path does, rather than appending a suffix after it.
/// Test: `service::reindex::hnsw_swap::tests`.
pub fn hnsw_staging_path(index_id: &str) -> Result<PathBuf> {
    Ok(index_data_dir(index_id)?.join("hnsw.reindex-staging.usearch"))
}

/// Resolve the HNSW snapshot path for `entry`, routing to colocated or legacy
/// storage based on `entry.colocated`.
///
/// Why: the persistence path helpers take only an `index_id` but colocated
/// indexes need the `root_path` to find `<root>/.trusty-search/hnsw.usearch`.
/// This helper unifies both cases so callers do not have to branch.
/// What: when `entry.colocated`, returns
/// `<root_path>/.trusty-search/hnsw.usearch`; otherwise delegates to `hnsw_path`.
/// Test: `colocated_hnsw_path_resolves_under_root` in `colocated_storage` tests.
pub fn hnsw_path_for_entry(entry: &PersistedIndex) -> Result<PathBuf> {
    if entry.colocated {
        crate::service::colocated_storage::colocated_hnsw_path(&entry.root_path)
    } else {
        hnsw_path(&entry.id)
    }
}

/// Resolve the redb corpus path for `entry`, routing to colocated or legacy
/// storage based on `entry.colocated`.
///
/// Why: see `hnsw_path_for_entry`.
/// What: when `entry.colocated`, returns
/// `<root_path>/.trusty-search/index.redb`; otherwise delegates to
/// `corpus_redb_path`.
/// Test: covered by colocated-index persistence integration tests.
pub fn corpus_redb_path_for_entry(entry: &PersistedIndex) -> Result<PathBuf> {
    if entry.colocated {
        crate::service::colocated_storage::colocated_redb_path(&entry.root_path)
    } else {
        corpus_redb_path(&entry.id)
    }
}

/// Resolve the schema-version stamp path for `entry`, routing to colocated or
/// legacy storage based on `entry.colocated`.
///
/// Why: see `hnsw_path_for_entry`.
/// What: when `entry.colocated`, returns
/// `<root_path>/.trusty-search/schema_version.json`; otherwise delegates to
/// `schema_version_path`.
/// Test: covered by colocated-index persistence integration tests.
pub fn schema_version_path_for_entry(entry: &PersistedIndex) -> Result<PathBuf> {
    if entry.colocated {
        crate::service::colocated_storage::colocated_schema_version_path(&entry.root_path)
    } else {
        schema_version_path(&entry.id)
    }
}

/// Resolve the staging redb corpus path for `entry`, routing to colocated or
/// legacy storage based on `entry.colocated`.
///
/// Why: see `hnsw_path_for_entry`.
/// What: when `entry.colocated`, returns
/// `<root_path>/.trusty-search/index.redb.tmp`; otherwise delegates to
/// `corpus_redb_tmp_path`.
/// Test: covered by colocated-index persistence integration tests.
pub fn corpus_redb_tmp_path_for_entry(entry: &PersistedIndex) -> Result<PathBuf> {
    if entry.colocated {
        crate::service::colocated_storage::colocated_redb_tmp_path(&entry.root_path)
    } else {
        corpus_redb_tmp_path(&entry.id)
    }
}

/// Load the registry file. Missing file → empty registry (first-run case).
///
/// Why: the daemon's `restore_indexes` startup hook calls this once. We treat
/// `NotFound` as "no indexes were ever registered" — not an error.
/// What: reads the TOML file, returns parsed entries. Corrupted file logs a
/// warning and returns empty so a bad save doesn't brick the daemon.
/// Test: `tests::registry_roundtrip` writes a file then loads it back.
pub fn load_index_registry() -> Result<Vec<PersistedIndex>> {
    load_index_registry_at(&indexes_toml_path()?)
}

/// Path-injectable variant of [`load_index_registry`]. Exists so the
/// roundtrip / delete-persistence tests can drive the load/save/upsert/remove
/// pipeline against a tempfile without monkey-patching `dirs::data_local_dir`.
pub fn load_index_registry_at(path: &Path) -> Result<Vec<PersistedIndex>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).context("read indexes.toml"),
    };
    match toml::from_str::<IndexRegistryFile>(&content) {
        Ok(file) => Ok(file.indexes),
        // #4317 / #4871: this arm used to return `Ok(Vec::new())` — an
        // unreadable registry read back as "no index was ever registered". The
        // very next write then went load(empty) → push one → save, overwriting a
        // real N-entry registry with a 1-entry file. That is the mass-
        // deregistration signature both incidents recorded (73 → 31, then 42 →
        // 5), and it is silent: every caller saw a successful write. An
        // unparseable registry is now an ERROR, so `upsert`/`remove` propagate
        // it and no save runs on top of a view that was never really read.
        Err(e) => {
            anyhow::bail!(
                "indexes.toml at {} could not be parsed ({e}); refusing to treat it as \
                 an empty registry — a write on top of that view would deregister every \
                 real index (#4317, #4871). Inspect or restore the file, then retry",
                path.display()
            )
        }
    }
}

/// Serializes every `indexes.toml` read-modify-write in this process (#4871).
///
/// Why: each mutation is a load → mutate → whole-file save, and nothing
/// ordered them. `search_handler` alone spawns a fire-and-forget
/// `update_last_queried_unix` per query, so a busy daemon runs many of these
/// concurrently against the same file, alongside registration handlers and the
/// boot reaper. Any write landing between another task's load and its save was
/// discarded with no error — the observed 80 → 88 → byte-identical-80 revert,
/// where 8 real registrations vanished and every caller reported success. The
/// original report blamed five concurrent daemons; the process census refuted
/// that, and this is the mechanism that survives it: one daemon racing itself.
/// What: a plain `Mutex<()>`. This is the one sanctioned `Lazy` static besides
/// the tracing subscriber — the lock must be process-wide to mean anything, and
/// threading it through every persistence caller would be a far larger change
/// for identical semantics.
/// Test: `concurrent_upserts_lose_no_entries`.
static REGISTRY_WRITE_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

/// Run `f` holding BOTH registry write locks — in-process and cross-process
/// (#4871, #5344).
///
/// Why: [`REGISTRY_WRITE_LOCK`] orders this process's own writers and nothing
/// else. `trusty-search prune`, `trusty-search prune-orphans` and
/// `trusty-search migrate storage` run as SEPARATE processes against the same
/// `indexes.toml` while a daemon is writing it — a session on 2026-08-05
/// observed five daemons doing so concurrently, last-writer-wins. An
/// in-process `Mutex` cannot see any of them, so #5344 adds the advisory
/// `indexes.toml.lock` sidecar that every writer, in every process, blocks on.
/// Callers must hold both across BOTH the load and the save, never just the
/// save — the discarded-write window is between them.
/// What: takes the process mutex first (recovering a poisoned guard, since the
/// data is a unit and a panicking holder cannot have corrupted it), then the
/// cross-process advisory lock via the shared
/// [`trusty_common::file_lock::with_exclusive_lock`] entry point, then runs
/// `f`. The acquisition order is the same at every call site, so the pair
/// cannot deadlock. A lock that cannot be acquired is an error — never a
/// bypass, because proceeding unlocked is the defect itself.
/// Test: `concurrent_upserts_lose_no_entries`; cross-process blocking is
/// exercised indirectly via
/// `commands::prune::tests::prune_apply_blocks_on_the_cross_process_registry_lock`
/// and
/// `commands::prune_orphans::tests::prune_orphans_blocks_on_the_cross_process_registry_lock`.
fn with_registry_write_lock<T>(path: &Path, f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _guard = REGISTRY_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    trusty_common::file_lock::with_exclusive_lock(path, f)
        .with_context(|| format!("acquire cross-process lock for {}", path.display()))?
}

/// Persist the registry atomically (write-tmp + rename) so a crash mid-write
/// never leaves a partially-written file.
pub fn save_index_registry(entries: &[PersistedIndex]) -> Result<()> {
    save_index_registry_at(&indexes_toml_path()?, entries)
}

/// Path-injectable variant of [`save_index_registry`].
///
/// #5344: takes the registry write lock (in-process AND cross-process) around
/// the publish, so a whole-file overwrite can no longer land in the middle of
/// another process's read-modify-write. Note what this does and does not buy:
/// the WRITE is now serialised, but a caller that loaded `entries` earlier is
/// still publishing a snapshot, so anything registered since that load is
/// erased. Prefer [`upsert_index_registry_entry_at`],
/// [`remove_index_registry_entries_at`] or [`patch_index_registry_entry_at`],
/// which re-read under the lock and mutate only what they own.
pub fn save_index_registry_at(path: &Path, entries: &[PersistedIndex]) -> Result<()> {
    with_registry_write_lock(path, || publish_registry_unlocked(path, entries))
}

/// Publish `entries` as the whole registry file — caller MUST already hold the
/// registry write lock.
///
/// Why: the locked read-modify-write helpers below call this from INSIDE their
/// critical section, and the cross-process lock (#5344) is not reentrant — a
/// nested acquisition on a second descriptor self-deadlocks. Splitting the
/// publish from the locking keeps `save_index_registry_at` safe for outside
/// callers without wedging the internal ones.
/// What: serialises to TOML, writes a per-write staging file, renames it over
/// `path`.
///
/// #4317: the staging file is unique per write (pid + a monotonic counter)
/// instead of the single shared `indexes.toml.tmp`. Two writers racing on one
/// fixed tmp path interleave their `write` calls, and the rename then publishes
/// a spliced, unparseable registry — which, before the fail-closed load above,
/// read back as an empty registry and took every real index with it.
/// Test: `registry_roundtrip` and every locked helper's test.
fn publish_registry_unlocked(path: &Path, entries: &[PersistedIndex]) -> Result<()> {
    let file = IndexRegistryFile {
        indexes: entries.to_vec(),
    };
    let serialized = toml::to_string_pretty(&file).context("serialize indexes.toml")?;
    static WRITE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = WRITE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("toml.tmp.{}.{seq}", std::process::id()));
    reap_stale_staging_files(path);
    std::fs::write(&tmp, serialized).context("write indexes.toml tmp")?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        // Never leave a uniquely-named staging file behind on a failed publish.
        let _ = std::fs::remove_file(&tmp);
        return Err(e).context("rename indexes.toml");
    }
    Ok(())
}

/// How long an abandoned `indexes.toml.tmp.*` survives before a later write
/// sweeps it. Comfortably longer than any real write, short enough that a crash
/// does not leave litter for the machine's lifetime.
const STAGING_STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(3600);

/// Delete `indexes.toml.tmp.*` files older than [`STAGING_STALE_AFTER`] (#4317).
///
/// Why (review LOW): per-write staging names fixed the shared-tmp collision but
/// removed the accidental cleanup the single fixed name provided — a crash
/// between `write` and `rename` now leaks a uniquely-named file that nothing
/// ever reclaims. Sweeping on write keeps the data dir bounded without adding a
/// boot-time pass or a ticker.
/// What: best-effort. Scans the registry file's parent for siblings whose name
/// starts with `<stem>.toml.tmp.` and unlinks those older than the window.
/// Every failure is swallowed — this is tidiness, and it must never fail a
/// registry write. Live files are protected by the age window, so a concurrent
/// writer's staging file is never removed out from under it.
/// Test: `stale_staging_files_are_reaped_fresh_ones_are_not`.
fn reap_stale_staging_files(registry_path: &Path) {
    let (Some(dir), Some(stem)) = (
        registry_path.parent(),
        registry_path.file_stem().and_then(|s| s.to_str()),
    ) else {
        return;
    };
    let prefix = format!("{stem}.toml.tmp.");
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in read_dir.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with(&prefix) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > STAGING_STALE_AFTER);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Append (or upsert) one entry to the registry file. Idempotent — re-adding
/// the same id replaces the previous entry's `root_path`.
///
/// Why: avoids a read-modify-write race when `POST /indexes` registers a new
/// index while the daemon's shutdown handler is concurrently flushing state.
/// What: load → upsert by id → save (atomically). Cheap; the file is tiny.
/// Test: `tests::registry_upsert_idempotent` covers re-registration.
pub fn upsert_index_registry_entry(entry: PersistedIndex) -> Result<()> {
    upsert_index_registry_entry_at(&indexes_toml_path()?, entry)
}

/// Path-injectable variant. Same upsert semantics, but reads/writes the
/// supplied TOML path. Used by the persistence tests (issue #118) to assert
/// that re-registering the same id never produces a duplicate `[[index]]`.
pub fn upsert_index_registry_entry_at(path: &Path, entry: PersistedIndex) -> Result<()> {
    // #4871: load and save must be one critical section — a registration that
    // lands between them is otherwise overwritten out of existence, silently.
    with_registry_write_lock(path, || {
        let mut entries = load_index_registry_at(path)?;
        if let Some(existing) = entries.iter_mut().find(|e| e.id == entry.id) {
            // Overwrite the whole record (not just root_path) so updated
            // `include_paths`/`exclude_globs`/`extensions`/`domain_terms` from
            // `trusty-search.yaml` flow through to disk on re-registration.
            *existing = entry;
        } else {
            entries.push(entry);
        }
        publish_registry_unlocked(path, &entries)
    })
}

/// Remove an entry from the registry file. Silently no-ops when the id is
/// absent (idempotent delete).
///
/// Why (issue #118): `DELETE /indexes/:id` evicts an index from the in-memory
/// `DashMap`, but unless the on-disk `indexes.toml` is also rewritten, the
/// next daemon restart re-registers the entry and pre-allocates an HNSW arena
/// for it — production saw 60+ "deleted" indexes accumulate this way and pin
/// 24 GB of RSS. This function is the persistence half of that fix; it is
/// called from `delete_index_handler` so the removal survives restart.
/// What: load → filter out `id` → atomic save. No-op when id absent.
/// Test: `tests::remove_index_persists_to_toml` registers two indexes, removes
/// one, reloads the file, asserts only the survivor remains.
pub fn remove_index_registry_entry(id: &str) -> Result<()> {
    remove_index_registry_entry_at(&indexes_toml_path()?, id)
}

/// Path-injectable variant of [`remove_index_registry_entry`].
pub fn remove_index_registry_entry_at(path: &Path, id: &str) -> Result<()> {
    remove_index_registry_entries_at(path, std::slice::from_ref(&id))
}

/// Remove many entries by id in ONE read-modify-write (#4317).
///
/// Why: the boot orphan-reaper used to publish its survivors with
/// `save_index_registry(&kept)` — a whole-file overwrite from a snapshot taken
/// before the daemon began serving. Any index registered while the reaper was
/// deciding was erased by that write, with the reaper's own log line reporting a
/// successful self-heal. Removing BY ID re-reads the current file under the lock
/// and drops only the ids actually judged orphaned, so a concurrent registration
/// survives the sweep instead of being collateral.
/// What: loads under the write lock, retains everything whose id is not in
/// `ids`, and saves only when something was actually dropped. Ids that are
/// absent are a no-op (idempotent delete).
/// Test: `boot_orphan_reap_preserves_a_registration_made_after_its_snapshot`,
/// `remove_entries_by_id_is_idempotent`.
pub fn remove_index_registry_entries_at(path: &Path, ids: &[&str]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    with_registry_write_lock(path, || {
        let mut entries = load_index_registry_at(path)?;
        let before = entries.len();
        entries.retain(|e| !ids.iter().any(|id| *id == e.id));
        if entries.len() == before {
            return Ok(());
        }
        publish_registry_unlocked(path, &entries)
    })
}

/// Remove many entries by id from the process-resolved registry path (#4317).
///
/// Why/What/Test: see [`remove_index_registry_entries_at`].
pub fn remove_index_registry_entries(ids: &[&str]) -> Result<()> {
    remove_index_registry_entries_at(&indexes_toml_path()?, ids)
}

/// Patch one existing entry in place, entirely inside the write lock (#4871).
///
/// Why (review MEDIUM): the LRU timestamp writers loaded the registry, found
/// their entry, mutated the clone, and only THEN called `upsert…`, which loads
/// again under the lock. The find happened OUTSIDE the critical section, so a
/// concurrent writer's change to a DIFFERENT field of the same entry (e.g.
/// `last_indexed_unix` while this one writes `last_queried_unix`) was
/// overwritten by the stale clone. Reading and writing under one lock makes
/// the read-modify-write actually atomic.
/// What: loads under the lock, applies `patch` to the entry whose id matches,
/// and saves. A missing id is `Ok(())` with no write — the entry may have been
/// deleted between the caller's decision and this call, which is harmless.
/// `patch` must not itself touch the registry (it runs while the lock is held).
/// Test: `patch_entry_is_atomic_against_a_concurrent_field_write`,
/// `patch_entry_for_a_missing_id_is_a_no_op`.
pub fn patch_index_registry_entry_at(
    path: &Path,
    id: &str,
    patch: impl FnOnce(&mut PersistedIndex),
) -> Result<()> {
    with_registry_write_lock(path, || {
        let mut entries = load_index_registry_at(path)?;
        let Some(entry) = entries.iter_mut().find(|e| e.id == id) else {
            return Ok(());
        };
        patch(entry);
        publish_registry_unlocked(path, &entries)
    })
}

/// Delete the on-disk data directory for an index (HNSW + chunks).
///
/// Why: paired with `DELETE /indexes/:id` so a removed index leaves no
/// residue. Failing to clean up isn't fatal — we log and continue.
/// What: best-effort recursive remove of `<data_dir>/indexes/<id>/`.
/// Test: create the dir, call this, assert it no longer exists.
pub fn remove_index_data_dir(index_id: &str) -> Result<()> {
    let dir = data_dir()?.join("indexes").join(sanitize_id(index_id));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
    }
    Ok(())
}

/// True iff a previously-saved HNSW snapshot exists on disk for this index.
pub fn has_persisted_hnsw(path: &Path) -> bool {
    path.exists() && path.is_file()
}

#[cfg(test)]
#[path = "persistence_tests.rs"]
mod tests;
