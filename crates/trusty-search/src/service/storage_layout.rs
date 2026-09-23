//! Registry-resolved on-disk storage layout for one index (#8438).
//!
//! Why: every write path used to choose its target by probing whether
//! `<root>/.trusty-search/` existed on disk, while every read path followed the
//! registry's `PersistedIndex::colocated` flag. An index registered
//! `colocated=false` whose root already held an in-repo `.trusty-search/` (for
//! example another host's live copy) therefore read from the data dir and wrote
//! into the repository — a split brain, and a write into a directory another
//! instance may be serving.
//! What: [`StorageLayout`] is decided once from the registry entry
//! ([`StorageLayout::for_entry`]) and carried on `CodeIndexer`.
//! [`StorageLayout::storage_dir`] is the single resolver every read and write
//! path goes through; the write guard ([`WriteUnderRootRefused`]) lives inside
//! it, so no path can reach the repository without the registry naming it.
//! Test: `service::storage_layout_8438_tests`.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

use crate::core::registry::IndexHandle;
use crate::service::colocated_storage::COLOCATED_DIR_NAME;
use crate::service::persistence::{self, PersistedIndex};

/// Live HNSW snapshot file name.
pub(crate) const HNSW_FILE: &str = "hnsw.usearch";
/// Staging HNSW snapshot written while a reindex runs (#3970).
pub(crate) const HNSW_STAGING_FILE: &str = "hnsw.reindex-staging.usearch";
/// Live redb corpus file name.
pub(crate) const REDB_FILE: &str = "index.redb";
/// Staging redb corpus written by a reindex (#28 Phase 4).
pub(crate) const REDB_TMP_FILE: &str = "index.redb.tmp";
/// Schema-version stamp file name.
pub(crate) const SCHEMA_VERSION_FILE: &str = "schema_version.json";
/// Legacy JSON chunk snapshot file name.
pub(crate) const CHUNKS_JSON_FILE: &str = "chunks.json";

/// Where one index keeps its on-disk state, as the registry records it.
///
/// Why: the registry is the only authority on where an index lives; the
/// presence of a `.trusty-search/` directory under the root is not.
/// What: `DataDir` → `<data_dir>/indexes/<id>/`; `Colocated` →
/// `<root_path>/.trusty-search/`. The default is `DataDir`, matching
/// `PersistedIndex::colocated`'s serde default.
/// Test: `layout_follows_the_registry_flag_not_the_directory`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum StorageLayout {
    #[default]
    DataDir,
    Colocated,
}

/// A resolution that would place a non-colocated index's storage under its
/// root was refused (#8438).
///
/// Why: a redirect would hide the misconfiguration; the write must fail so the
/// caller logs it and skips the write.
/// What: carried inside the `anyhow::Error` the resolver returns; callers can
/// recognise it with [`is_write_refusal`].
/// Test: `guard_refuses_a_data_dir_that_resolves_into_the_repo`.
#[derive(Debug, thiserror::Error)]
#[error(
    "refusing to place storage for index '{index_id}' at {}: it lies under the index \
     root {} but the registry entry is not colocated (#8438)",
    target.display(),
    root.display()
)]
pub(crate) struct WriteUnderRootRefused {
    pub(crate) index_id: String,
    pub(crate) target: PathBuf,
    pub(crate) root: PathBuf,
}

/// A colocated resolution whose index root does not exist (#8438).
///
/// Why: creating `<root>/.trusty-search` with `create_dir_all` recreated a
/// deleted root, so the #484 missing-root guard at warm boot passed and a
/// later `git worktree add` at that path failed. A missing root is an error;
/// the caller logs it and skips the write.
/// What: carried inside the `anyhow::Error` the resolver returns; callers can
/// recognise it with [`is_write_refusal`].
/// Test: `colocated_persist_never_recreates_a_missing_root`,
/// `shutdown_flush_never_recreates_a_missing_colocated_root`.
#[derive(Debug, thiserror::Error)]
#[error(
    "refusing to place storage for colocated index '{index_id}': its root {} does not \
     exist, and a write must never recreate it (#8438)",
    root.display()
)]
pub(crate) struct ColocatedRootMissing {
    pub(crate) index_id: String,
    pub(crate) root: PathBuf,
}

/// True when `e` is (or wraps) a resolver refusal — [`WriteUnderRootRefused`]
/// or [`ColocatedRootMissing`]. Callers log a refusal at error and skip.
pub(crate) fn is_write_refusal(e: &anyhow::Error) -> bool {
    e.downcast_ref::<WriteUnderRootRefused>().is_some()
        || e.downcast_ref::<ColocatedRootMissing>().is_some()
}

impl StorageLayout {
    /// The layout the registry entry names.
    pub(crate) fn for_entry(entry: &PersistedIndex) -> Self {
        if entry.colocated {
            Self::Colocated
        } else {
            Self::DataDir
        }
    }

    /// Resolve (and create) this index's storage directory — the one resolver.
    ///
    /// Why: see the module docs; every path helper below and every write site
    /// in the crate funnels through here, so the layout decision and the guard
    /// exist exactly once.
    /// What: `Colocated` → `<root_path>/.trusty-search/`, created with a
    /// non-recursive `create_dir`, so a missing root is a
    /// [`ColocatedRootMissing`] error and is never recreated. `DataDir` →
    /// `<data_dir>/indexes/<sanitized id>/`, checked by
    /// [`refuse_write_under_root`] before `indexes/<id>/` is created. A refused
    /// `DataDir` resolution never creates `indexes/<id>/`, but
    /// `persistence::data_dir()` runs first and creates the configured data
    /// dir itself (`TRUSTY_DATA_DIR`, or the platform default) when missing —
    /// so a `TRUSTY_DATA_DIR` naming a path inside the repo leaves that one
    /// directory behind.
    /// Test: `guard_refuses_a_data_dir_that_resolves_into_the_repo`,
    /// `colocated_layout_creates_a_missing_directory`,
    /// `colocated_persist_never_recreates_a_missing_root`.
    pub(crate) fn storage_dir(self, index_id: &str, root_path: &Path) -> Result<PathBuf> {
        match self {
            // #8438: never `create_dir_all` — that recreated a deleted root.
            Self::Colocated => colocated_dir_under_existing_root(index_id, root_path),
            Self::DataDir => {
                // #8438: creates the data dir itself before the guard runs;
                // see the doc comment above.
                let base = persistence::data_dir()?;
                let dir = base
                    .join("indexes")
                    .join(persistence::sanitize_id_for_path(index_id));
                refuse_write_under_root(index_id, root_path, &base, &dir)?;
                std::fs::create_dir_all(&dir).context("create per-index data dir")?;
                Ok(dir)
            }
        }
    }

    /// `storage_dir(..)/<name>`.
    pub(crate) fn file(self, index_id: &str, root_path: &Path, name: &str) -> Result<PathBuf> {
        Ok(self.storage_dir(index_id, root_path)?.join(name))
    }

    /// Remove this index's storage without creating anything (#8438).
    ///
    /// Why: `delete_index?delete_data=true` removed only the data dir, so a
    /// colocated index kept its `<root>/.trusty-search/` forever; and a
    /// non-colocated index must never remove the in-repo directory, which may
    /// be another instance's live data.
    /// What: always removes `<data_dir>/indexes/<id>/` (legacy residue);
    /// when colocated, also removes trusty-search's own index files from
    /// `<root>/.trusty-search/` ([`remove_own_index_files`]), then the
    /// directory itself only if that left it empty.
    /// Test: `delete_data_removes_the_directory_the_registry_names`,
    /// `delete_data_keeps_foreign_files_in_a_shared_trusty_search_dir`.
    pub(crate) fn remove_storage(self, index_id: &str, root_path: Option<&Path>) -> Result<()> {
        persistence::remove_index_data_dir(index_id)?;
        if let (Self::Colocated, Some(root)) = (self, root_path) {
            // #8438: never `remove_dir_all` — `$HOME/.trusty-search/` is also
            // the daemon's runtime dir (config.toml, http_addr, mcp_http_addr).
            remove_own_index_files(&root.join(COLOCATED_DIR_NAME))?;
        }
        Ok(())
    }
}

/// File-name stems of every artifact trusty-search writes into an index's
/// storage directory: the live files, their staging and `.tmp` siblings, the
/// HNSW key sidecar, and the `.corrupt[.N]` / incompatible-format backups.
const OWN_FILE_STEMS: [&str; 4] = ["hnsw.", REDB_FILE, SCHEMA_VERSION_FILE, CHUNKS_JSON_FILE];

/// True when `name` is one of trusty-search's own index files.
fn is_own_index_file(name: &str) -> bool {
    OWN_FILE_STEMS.iter().any(|stem| name.starts_with(stem))
}

/// Remove trusty-search's index files from `dir`, then `dir` if it is empty.
///
/// Why (#8438): a colocated index rooted at `$HOME` shares
/// `$HOME/.trusty-search/` with the daemon's own runtime files; deleting the
/// index must not delete those.
/// What: deletes every regular file in `dir` whose name starts with one of
/// [`OWN_FILE_STEMS`], leaves every other entry, then removes `dir` only when
/// nothing is left. A missing `dir` is a no-op.
/// Test: `delete_data_keeps_foreign_files_in_a_shared_trusty_search_dir`.
fn remove_own_index_files(dir: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("read {}", dir.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("read {}", dir.display()))?;
        let own = entry.file_name().to_str().is_some_and(is_own_index_file);
        if own && entry.file_type().is_ok_and(|t| t.is_file()) {
            let path = entry.path();
            std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
    }
    match std::fs::remove_dir(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove {}", dir.display())),
    }
}

/// Resolve `<root>/.trusty-search/`, creating only that last component.
///
/// Why (#8438): `create_dir_all` recreated a deleted root; see
/// [`ColocatedRootMissing`].
/// What: a non-recursive `create_dir`; an existing directory is accepted, a
/// missing or empty root is [`ColocatedRootMissing`], and any other failure is
/// returned with context. Nothing outside `<root>` is ever created.
/// Test: `colocated_persist_never_recreates_a_missing_root`,
/// `colocated_layout_creates_a_missing_directory`.
fn colocated_dir_under_existing_root(index_id: &str, root: &Path) -> Result<PathBuf> {
    let missing = || -> anyhow::Error {
        ColocatedRootMissing {
            index_id: index_id.to_string(),
            root: root.to_path_buf(),
        }
        .into()
    };
    if root.as_os_str().is_empty() {
        return Err(missing());
    }
    let dir = root.join(COLOCATED_DIR_NAME);
    match std::fs::create_dir(&dir) {
        Ok(()) => Ok(dir),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(dir),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(missing()),
        Err(e) => {
            Err(e).with_context(|| format!("create colocated storage dir at {}", dir.display()))
        }
    }
}

/// The write guard: refuse a `DataDir` target that lands under the root.
///
/// Why (#8438): nothing may be written under `<root>/.trusty-search` unless the
/// registry names that directory for the index.
/// What: returns [`WriteUnderRootRefused`] when `target` lies under
/// `<root>/.trusty-search`, or under `root` at all while the configured data
/// dir `data_base` does not. The second clause exempts an operator whose data
/// dir itself sits under the root (for example an index rooted at `$HOME`);
/// the in-repo `.trusty-search` stays refused in every case.
/// Test: `guard_refuses_a_data_dir_that_resolves_into_the_repo`.
fn refuse_write_under_root(
    index_id: &str,
    root: &Path,
    data_base: &Path,
    target: &Path,
) -> Result<()> {
    if root.as_os_str().is_empty() {
        return Ok(());
    }
    let in_repo_dir = target.starts_with(root.join(COLOCATED_DIR_NAME));
    let under_root = target.starts_with(root) && !data_base.starts_with(root);
    if in_repo_dir || under_root {
        return Err(WriteUnderRootRefused {
            index_id: index_id.to_string(),
            target: target.to_path_buf(),
            root: root.to_path_buf(),
        }
        .into());
    }
    Ok(())
}

/// Read the layout carried by `handle`'s indexer.
///
/// Why: write sites that hold only an `IndexHandle` (reindex swaps, shutdown,
/// migrations, delete) need the layout without a field on the all-pub
/// `IndexHandle`, which would force a semver-major release.
/// What: a short read lock on the indexer. Callers must not already hold an
/// indexer guard.
pub(crate) async fn layout_of(handle: &IndexHandle) -> StorageLayout {
    handle.indexer.read().await.storage_layout()
}

/// Resolve `name` inside `handle`'s registry-named storage directory.
pub(crate) async fn handle_file(handle: &IndexHandle, name: &str) -> Result<PathBuf> {
    layout_of(handle)
        .await
        .file(&handle.id.0, &handle.root_path, name)
}

#[cfg(test)]
#[path = "storage_layout_8438_tests.rs"]
pub(crate) mod storage_layout_8438_tests;
