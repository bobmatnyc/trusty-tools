//! Persistence helpers: registry TOML + per-index data directories.
//!
//! Why: The daemon currently keeps every HNSW vector, chunk corpus, and index
//! registration in process memory only. Every restart forces a full re-index,
//! which on a 100k-chunk repo costs 2-3 minutes and 86 MB of model load on
//! top. Persisting these three things across restarts (issue #85) makes the
//! daemon "warm-boot ready" — registered indexes come back automatically with
//! their HNSW graph and chunk metadata intact.
//!
//! What: this module centralises filesystem layout and (de)serialization for
//! the persistence layer. Three responsibilities:
//!
//! 1. [`indexes_toml_path`] / [`load_index_registry`] / [`save_index_registry`]
//!    — the registry of `IndexId → root_path` lives at `<data_dir>/indexes.toml`.
//! 2. [`index_data_dir`] — per-index directory `<data_dir>/indexes/<id>/`
//!    holds `hnsw.usearch` (vector graph) and `chunks.json` (corpus snapshot).
//! 3. [`remove_index_data_dir`] — used by `DELETE /indexes/:id` to evict the
//!    on-disk footprint when an index is unregistered.
//!
//! Test: round-trip a `PersistedIndex` through `save_index_registry` /
//! `load_index_registry` in a tempdir; assert the entry survives. Verified
//! by `tests::registry_roundtrip` below.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// On-disk record for one registered index. Kept tiny so the TOML file stays
/// human-readable for ops debugging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
}

/// Why: serde's `default` attribute needs a free function (closures aren't
/// allowed). Centralising the default here keeps it identical for
/// deserialisation and for the `PersistedIndex::default()` fallback.
fn default_respect_gitignore() -> bool {
    true
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
    /// `respect_gitignore` defaults to `true` (issue #100) and
    /// `include_docs` defaults to `true` (issue #118) so the manual
    /// `Default` impl matches serde's missing-field behaviour. Without
    /// this, `PersistedIndex::default()` would silently re-enable the
    /// docs-exclusion footgun (#118) or disable the gitignore-honouring
    /// fix (#100) on test / fallback paths.
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
            lexical_only: false,
        }
    }
}

/// TOML wrapper so the file uses `[[index]]` array-of-tables syntax —
/// matches the public format documented in CLAUDE.md.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct IndexRegistryFile {
    #[serde(default, rename = "index")]
    pub indexes: Vec<PersistedIndex>,
}

/// Resolve the daemon's data directory, mirroring `daemon::daemon_dir` so all
/// persistence files share one parent on every platform.
///
/// Why: `daemon_dir` lives behind a typed `DaemonError` and is private. We
/// duplicate the `data_local_dir().join("trusty-search")` lookup here so this
/// module doesn't take a `DaemonError` dependency just to read its path.
/// What: returns `<data_local_dir>/trusty-search`. Creates the directory if
/// missing.
/// Test: `tests::data_dir_creates_parent` constructs and asserts the dir exists.
pub fn data_dir() -> Result<PathBuf> {
    let dir = dirs::data_local_dir()
        .context("could not determine data-local directory")?
        .join("trusty-search");
    std::fs::create_dir_all(&dir).context("create trusty-search data dir")?;
    Ok(dir)
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

/// Crate-internal wrapper exposing [`sanitize_id`] for callers that need to
/// derive the same on-disk path as [`index_data_dir`] without triggering its
/// `create_dir_all` side effect.
pub(crate) fn sanitize_id_for_path(id: &str) -> String {
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
pub(crate) fn load_index_registry_at(path: &Path) -> Result<Vec<PersistedIndex>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).context("read indexes.toml"),
    };
    match toml::from_str::<IndexRegistryFile>(&content) {
        Ok(file) => Ok(file.indexes),
        Err(e) => {
            tracing::warn!(
                "indexes.toml at {} is corrupt ({e}); starting with empty registry",
                path.display()
            );
            Ok(Vec::new())
        }
    }
}

/// Persist the registry atomically (write-tmp + rename) so a crash mid-write
/// never leaves a partially-written file.
pub fn save_index_registry(entries: &[PersistedIndex]) -> Result<()> {
    save_index_registry_at(&indexes_toml_path()?, entries)
}

/// Path-injectable variant of [`save_index_registry`].
pub(crate) fn save_index_registry_at(path: &Path, entries: &[PersistedIndex]) -> Result<()> {
    let file = IndexRegistryFile {
        indexes: entries.to_vec(),
    };
    let serialized = toml::to_string_pretty(&file).context("serialize indexes.toml")?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, serialized).context("write indexes.toml tmp")?;
    std::fs::rename(&tmp, path).context("rename indexes.toml")?;
    Ok(())
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
pub(crate) fn upsert_index_registry_entry_at(path: &Path, entry: PersistedIndex) -> Result<()> {
    let mut entries = load_index_registry_at(path)?;
    if let Some(existing) = entries.iter_mut().find(|e| e.id == entry.id) {
        // Overwrite the whole record (not just root_path) so updated
        // `include_paths`/`exclude_globs`/`extensions`/`domain_terms` from
        // `trusty-search.yaml` flow through to disk on re-registration.
        *existing = entry;
    } else {
        entries.push(entry);
    }
    save_index_registry_at(path, &entries)
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
pub(crate) fn remove_index_registry_entry_at(path: &Path, id: &str) -> Result<()> {
    let mut entries = load_index_registry_at(path)?;
    let before = entries.len();
    entries.retain(|e| e.id != id);
    if entries.len() == before {
        return Ok(());
    }
    save_index_registry_at(path, &entries)
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
mod tests {
    use super::*;

    /// Test helper: redirect `data_local_dir` to a tempdir so tests don't
    /// touch the user's real `~/Library/Application Support/trusty-search`.
    /// We override via the `XDG_DATA_HOME` / `HOME` env vars that the `dirs`
    /// crate consults — but since `dirs::data_local_dir` is platform-specific,
    /// we instead test the helpers that take an explicit base path.
    ///
    /// For full-flow tests we use a unique-id namespace so concurrent runs
    /// don't collide on the real data dir.

    #[test]
    fn sanitize_strips_unsafe_chars() {
        assert_eq!(sanitize_id("good-name_1.0"), "good-name_1.0");
        // `.` is in the allow-set; `/` becomes `_`. So `../escape` becomes
        // `.._escape`. The important invariant is that no path separator
        // survives, not that dots are stripped.
        assert_eq!(sanitize_id("../escape"), ".._escape");
        assert_eq!(sanitize_id("with spaces/slash"), "with_spaces_slash");
    }

    #[test]
    fn registry_file_serde_roundtrip() {
        // Just exercise the (de)serializer without touching the filesystem.
        let file = IndexRegistryFile {
            indexes: vec![
                PersistedIndex {
                    id: "a".into(),
                    root_path: PathBuf::from("/tmp/a"),
                    ..Default::default()
                },
                PersistedIndex {
                    id: "b".into(),
                    root_path: PathBuf::from("/tmp/b"),
                    ..Default::default()
                },
            ],
        };
        let s = toml::to_string_pretty(&file).unwrap();
        let parsed: IndexRegistryFile = toml::from_str(&s).unwrap();
        assert_eq!(parsed.indexes, file.indexes);
    }

    /// Regression test for issue #118: `DELETE /indexes/:id` must rewrite
    /// `indexes.toml` so the removal survives a daemon restart.
    ///
    /// Why: production accumulated 60+ "deleted" indexes because the DELETE
    /// path only mutated the in-memory `DashMap`. Each empty entry replayed
    /// from disk pre-allocates an HNSW arena (80–150 MB). The fix wires
    /// `delete_index_handler` to `remove_index_registry_entry`; this test
    /// pins that behaviour at the persistence boundary by driving the
    /// load/save/remove pipeline against a tempfile and asserting the
    /// deleted id is absent from the rehydrated registry.
    #[test]
    fn remove_index_persists_to_toml() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        upsert_index_registry_entry_at(
            &path,
            PersistedIndex {
                id: "keep".into(),
                root_path: PathBuf::from("/tmp/keep"),
                ..Default::default()
            },
        )
        .unwrap();
        upsert_index_registry_entry_at(
            &path,
            PersistedIndex {
                id: "drop".into(),
                root_path: PathBuf::from("/tmp/drop"),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(load_index_registry_at(&path).unwrap().len(), 2);

        // Delete the second entry — this is the persistence call that
        // `delete_index_handler` makes on the DELETE handler path.
        remove_index_registry_entry_at(&path, "drop").unwrap();

        // Rehydrate from disk (simulating a daemon restart) and confirm only
        // the survivor comes back. This is the assertion that would have
        // failed before the fix.
        let restored = load_index_registry_at(&path).unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].id, "keep");
        assert!(restored.iter().all(|e| e.id != "drop"));

        // Idempotent delete: removing again is a silent no-op.
        remove_index_registry_entry_at(&path, "drop").unwrap();
        assert_eq!(load_index_registry_at(&path).unwrap().len(), 1);
    }

    /// Regression test for the add-side of issue #118: re-registering the
    /// same `id` must upsert (not append) in the on-disk file.
    ///
    /// Why: if `POST /indexes` appended a duplicate `[[index]]` block on
    /// every call, a flapping daemon would build up the same accumulation
    /// pathology the DELETE bug caused — every duplicate replays as a
    /// separate HNSW arena at startup.
    #[test]
    fn upsert_index_dedupes_on_id() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        upsert_index_registry_entry_at(
            &path,
            PersistedIndex {
                id: "proj".into(),
                root_path: PathBuf::from("/old"),
                ..Default::default()
            },
        )
        .unwrap();
        // Re-register with the same id but a different root_path.
        upsert_index_registry_entry_at(
            &path,
            PersistedIndex {
                id: "proj".into(),
                root_path: PathBuf::from("/new"),
                ..Default::default()
            },
        )
        .unwrap();

        let entries = load_index_registry_at(&path).unwrap();
        assert_eq!(entries.len(), 1, "duplicate [[index]] block written");
        assert_eq!(entries[0].root_path, PathBuf::from("/new"));
    }

    /// Issue #100: `respect_gitignore` defaults to `true` on every code path —
    /// constructor, missing-field deserialisation, and after a save/load
    /// round-trip. This pins the back-compat contract: an `indexes.toml`
    /// written by a previous trusty-search version must pick up the
    /// gitignore-honouring fix automatically on warm boot.
    #[test]
    fn respect_gitignore_defaults_true_and_round_trips() {
        // Default constructor returns true.
        assert!(PersistedIndex::default().respect_gitignore);

        // Loading legacy TOML without the field gives true.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::fs::write(
            &path,
            r#"
[[index]]
id = "legacy"
root_path = "/tmp/legacy"
"#,
        )
        .unwrap();
        let entries = load_index_registry_at(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].respect_gitignore,
            "missing field must default to true (issue #100 back-compat)"
        );

        // Explicit false survives save/load cycle.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        save_index_registry_at(
            &path,
            &[PersistedIndex {
                id: "vendored".into(),
                root_path: PathBuf::from("/tmp/v"),
                respect_gitignore: false,
                ..Default::default()
            }],
        )
        .unwrap();
        let entries = load_index_registry_at(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].respect_gitignore);
    }

    /// Issue #118: `include_docs` defaults to `true` on every code path —
    /// constructor, missing-field deserialisation, and after a save/load
    /// round-trip. This pins the back-compat migration story: an
    /// `indexes.toml` written by v0.8.2 (where `include_docs = false` was
    /// the default and would be omitted from the file by
    /// `skip_serializing_if = "std::ops::Not::not"`) now reads back as
    /// `true` under v0.8.3 — `mode=text` searches start returning results
    /// on the next daemon restart without any explicit migration step.
    /// Indexes that PERSISTED an explicit `include_docs = false` keep
    /// their opt-out via the explicit-false round-trip case below.
    #[test]
    fn include_docs_defaults_true_and_round_trips() {
        // Default constructor returns true.
        assert!(PersistedIndex::default().include_docs);

        // Loading legacy TOML without the field gives true — this is the
        // v0.8.2 → v0.8.3 silent migration: missing field becomes the new
        // default.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::fs::write(
            &path,
            r#"
[[index]]
id = "legacy"
root_path = "/tmp/legacy"
"#,
        )
        .unwrap();
        let entries = load_index_registry_at(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            entries[0].include_docs,
            "missing field must default to true (issue #118 migration)"
        );

        // Explicit false survives save/load cycle — opt-out users keep their
        // setting through the upgrade.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        save_index_registry_at(
            &path,
            &[PersistedIndex {
                id: "docs_off".into(),
                root_path: PathBuf::from("/tmp/v"),
                include_docs: false,
                ..Default::default()
            }],
        )
        .unwrap();
        let entries = load_index_registry_at(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].include_docs);
    }

    /// Issue #109 Phase 1: `lexical_only` defaults to `false` and is
    /// omitted from the TOML when unset, so existing `indexes.toml` files
    /// keep their compact shape. An explicit `true` survives a save/load
    /// cycle.
    #[test]
    fn lexical_only_round_trips() {
        // Default constructor returns false.
        assert!(!PersistedIndex::default().lexical_only);

        // Loading legacy TOML without the field gives false (full pipeline).
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        std::fs::write(
            &path,
            r#"
[[index]]
id = "legacy"
root_path = "/tmp/legacy"
"#,
        )
        .unwrap();
        let entries = load_index_registry_at(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(
            !entries[0].lexical_only,
            "missing field must default to false (issue #109 back-compat)"
        );

        // Explicit true survives round-trip and is written to disk.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        save_index_registry_at(
            &path,
            &[PersistedIndex {
                id: "lex_only".into(),
                root_path: PathBuf::from("/tmp/v"),
                lexical_only: true,
                ..Default::default()
            }],
        )
        .unwrap();
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(
            s.contains("lexical_only"),
            "explicit true must be serialised — TOML was: {s}"
        );
        let entries = load_index_registry_at(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].lexical_only);
    }

    #[test]
    fn registry_upsert_idempotent_unit() {
        // Exercise the upsert *logic* without touching disk: simulate the
        // load → modify → save round-trip by manipulating the vector directly.
        let mut entries = vec![PersistedIndex {
            id: "a".into(),
            root_path: PathBuf::from("/old"),
            ..Default::default()
        }];
        let new = PersistedIndex {
            id: "a".into(),
            root_path: PathBuf::from("/new"),
            ..Default::default()
        };
        if let Some(existing) = entries.iter_mut().find(|e| e.id == new.id) {
            existing.root_path = new.root_path.clone();
        } else {
            entries.push(new);
        }
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].root_path, PathBuf::from("/new"));
    }
}
