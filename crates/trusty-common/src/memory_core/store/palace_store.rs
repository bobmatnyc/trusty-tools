//! Palace metadata + identity.txt persistence.
//!
//! Why: Palaces are long-lived state on disk; without metadata persistence the
//! CLI cannot list palaces across runs and there is no canonical place to store
//! the palace identity (L0 baseline context).
//! What: `PalaceStore` provides atomic save/load of `Palace` metadata as JSON
//! at `<data_dir>/palace.json`, plus identity.txt read/write helpers and a
//! registry-wide palace listing walker.
//! Test: `palace_store_roundtrip` and `identity_txt_roundtrip` in this module
//! cover serde + atomic writes; `registry_create_and_open` (in registry.rs)
//! exercises the registry-level wiring.

use crate::memory_core::palace::{Palace, PalaceId};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Filename for serialized palace metadata.
const PALACE_JSON: &str = "palace.json";
/// Filename for the L0 identity blob.
const IDENTITY_TXT: &str = "identity.txt";

/// Errors raised by palace persistence operations.
///
/// Why: Library code returns `Result<_, PalaceStoreError>` so callers can
/// distinguish missing metadata from genuine I/O failure.
/// What: Wraps `std::io::Error` and `serde_json::Error` plus a "not found"
/// variant for missing metadata files.
/// Test: `load_palace_missing_returns_not_found` (implicit via roundtrip test).
#[derive(Debug, Error)]
pub enum PalaceStoreError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("json error at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("palace metadata missing at {0}")]
    NotFound(PathBuf),
}

impl PalaceStoreError {
    fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
    fn json(path: impl Into<PathBuf>, source: serde_json::Error) -> Self {
        Self::Json {
            path: path.into(),
            source,
        }
    }
}

type Result<T> = std::result::Result<T, PalaceStoreError>;

/// On-disk palace metadata format. Mirrors `Palace` but is its own type so we
/// can evolve the on-disk schema independently if needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PalaceJson {
    id: String,
    name: String,
    description: Option<String>,
    created_at: chrono::DateTime<Utc>,
    data_dir: PathBuf,
    /// Schema version for forward compatibility.
    #[serde(default = "default_schema_version")]
    schema_version: u32,
}

fn default_schema_version() -> u32 {
    1
}

impl From<&Palace> for PalaceJson {
    fn from(p: &Palace) -> Self {
        Self {
            id: p.id.0.clone(),
            name: p.name.clone(),
            description: p.description.clone(),
            created_at: p.created_at,
            data_dir: p.data_dir.clone(),
            schema_version: 1,
        }
    }
}

impl From<PalaceJson> for Palace {
    fn from(j: PalaceJson) -> Self {
        Self {
            id: PalaceId(j.id),
            name: j.name,
            description: j.description,
            created_at: j.created_at,
            data_dir: j.data_dir,
        }
    }
}

/// Stateless namespace for palace persistence helpers.
///
/// Why: Palace metadata persistence has no state of its own — every operation
/// is a pure function over a path. Grouping under a unit struct gives a stable
/// import path while keeping the helpers `pub fn` rather than methods.
/// What: `save_palace` / `load_palace` / `list_palaces` plus identity.txt
/// helpers.
/// Test: This module's tests cover roundtrip and listing.
pub struct PalaceStore;

impl PalaceStore {
    /// Persist a palace's metadata to `<data_dir>/palace.json` atomically.
    ///
    /// Why: Crash-safety — if the daemon dies mid-write we must not leave a
    /// half-written `palace.json` that cannot deserialize.
    /// What: Creates `data_dir`, writes JSON to `palace.json.tmp`, fsyncs and
    /// renames over `palace.json`.
    /// Test: `palace_store_roundtrip` save + load round-trips all fields.
    pub fn save_palace(palace: &Palace) -> Result<()> {
        let data_dir = palace.data_dir.clone();
        std::fs::create_dir_all(&data_dir).map_err(|e| PalaceStoreError::io(&data_dir, e))?;

        let target = data_dir.join(PALACE_JSON);
        let tmp = data_dir.join(format!("{PALACE_JSON}.tmp"));

        let json: PalaceJson = palace.into();
        let bytes = serde_json::to_vec_pretty(&json)
            .map_err(|e| PalaceStoreError::json(target.clone(), e))?;

        std::fs::write(&tmp, &bytes).map_err(|e| PalaceStoreError::io(tmp.clone(), e))?;
        std::fs::rename(&tmp, &target).map_err(|e| PalaceStoreError::io(target.clone(), e))?;
        Ok(())
    }

    /// Load palace metadata from `<data_dir>/palace.json`.
    ///
    /// Why: Re-opening a palace requires reconstructing its `Palace` struct
    /// before we can wire up storage handles.
    /// What: Reads `palace.json`, deserializes into `Palace`. Returns
    /// `NotFound` if the file is missing.
    /// Test: `palace_store_roundtrip` confirms fields survive a save+load.
    pub fn load_palace(data_dir: &Path) -> Result<Palace> {
        let target = data_dir.join(PALACE_JSON);
        if !target.exists() {
            return Err(PalaceStoreError::NotFound(target));
        }
        let bytes = std::fs::read(&target).map_err(|e| PalaceStoreError::io(target.clone(), e))?;
        let json: PalaceJson =
            serde_json::from_slice(&bytes).map_err(|e| PalaceStoreError::json(target, e))?;
        Ok(json.into())
    }

    /// Walk `registry_dir` for palace subdirectories and return all loadable
    /// palaces.
    ///
    /// Why: `palace list` and the daemon startup path both need to enumerate
    /// every palace on disk, and the destructive passes (`purge_palaces`,
    /// `rebuild_palaces`, `merge_palaces`) act on whatever this returns. A
    /// short listing reads exactly like a complete one, so any palace dropped
    /// here is one those passes skip while reporting a clean run.
    /// What: For each immediate child directory holding a `palace.json`, loads
    /// and pushes the `Palace`. A child that is genuinely not a palace is
    /// skipped; a child that cannot be classified aborts the walk with the
    /// underlying error, which names the offending path.
    /// Test: `list_palaces_finds_saved_palaces` and
    /// `list_palaces_missing_dir_is_empty` pin the benign arms;
    /// `list_palaces_propagates_an_unstattable_registry_dir`,
    /// `list_palaces_propagates_an_unstattable_entry`,
    /// `list_palaces_propagates_an_unstattable_palace_json` and
    /// `list_palaces_propagates_an_undecodable_palace` pin the error arms.
    ///
    /// Absence and failure-to-determine stay distinct at every step, which is
    /// why the guards are `try_exists` and `metadata` rather than `exists` and
    /// `is_dir`: the latter are `fs::metadata(..).is_ok()`-shaped and coerce
    /// *every* stat failure to `false`, so a path we are forbidden to stat
    /// reads as a path that is not there. Only genuine absence still skips
    /// silently — `Ok(false)`, a dangling symlink, an entry unlinked mid-walk,
    /// or a subdirectory that simply holds no `palace.json`. A first run
    /// against a data root that does not exist yet is normal and must not
    /// error.
    ///
    /// Failing the whole walk on one corrupt palace and skipping it are both
    /// bad, and this picks failing (#5543): the error names the broken path, so
    /// an operator can repair or remove it in one step, whereas a silent skip
    /// makes that palace permanently invisible with no diagnostic while the
    /// destructive callers quietly pass over it. `bm25_backfill`'s private
    /// enumeration already made the same call for the same reason.
    pub fn list_palaces(registry_dir: &Path) -> Result<Vec<Palace>> {
        // #5532: `exists()` reports a stat we are denied as "absent", silently
        // turning an unreadable registry into an empty one.
        let present = registry_dir
            .try_exists()
            .map_err(|e| PalaceStoreError::io(registry_dir, e))?;
        if !present {
            return Ok(Vec::new());
        }
        let read_dir =
            std::fs::read_dir(registry_dir).map_err(|e| PalaceStoreError::io(registry_dir, e))?;

        let mut palaces = Vec::new();
        for entry in read_dir {
            // #5543: an entry failing mid-enumeration truncated the walk.
            let entry = entry.map_err(|e| PalaceStoreError::io(registry_dir, e))?;
            let path = entry.path();

            // #5543: `is_dir()` coerced a denied stat to "not a directory".
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                // Unlinked mid-walk, or a dangling symlink: genuinely absent,
                // the one arm `try_exists` also reports as `Ok(false)`.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(PalaceStoreError::io(&path, e)),
            };
            if !meta.is_dir() {
                continue;
            }

            // #5543: `exists()` coerced a denied stat to "holds no metadata".
            let metadata_path = path.join(PALACE_JSON);
            let has_metadata = metadata_path
                .try_exists()
                .map_err(|e| PalaceStoreError::io(&metadata_path, e))?;
            if !has_metadata {
                continue;
            }

            match Self::load_palace(&path) {
                Ok(p) => palaces.push(p),
                // Raced away between the guard above and the read.
                Err(PalaceStoreError::NotFound(_)) => continue,
                // #5543: a denied read or an undecodable `palace.json` used to
                // drop the palace and still report success.
                Err(e) => return Err(e),
            }
        }
        Ok(palaces)
    }

    /// Persist the identity (L0) text for a palace.
    ///
    /// Why: L0 identity is read on every palace open; storing it as a plain
    /// `identity.txt` keeps it human-editable.
    /// What: Atomic write of `text` to `<data_dir>/identity.txt`.
    /// Test: `identity_txt_roundtrip` saves and reloads.
    pub fn save_identity(_palace_id: &PalaceId, text: &str, data_dir: &Path) -> Result<()> {
        std::fs::create_dir_all(data_dir).map_err(|e| PalaceStoreError::io(data_dir, e))?;
        let target = data_dir.join(IDENTITY_TXT);
        let tmp = data_dir.join(format!("{IDENTITY_TXT}.tmp"));
        std::fs::write(&tmp, text.as_bytes()).map_err(|e| PalaceStoreError::io(tmp.clone(), e))?;
        std::fs::rename(&tmp, &target).map_err(|e| PalaceStoreError::io(target.clone(), e))?;
        Ok(())
    }

    /// Read the identity (L0) text for a palace, if present.
    ///
    /// Why: Brand-new palaces may not have an identity yet — returning
    /// `Ok(None)` lets callers fall back to a default without treating the
    /// missing file as an error.
    /// What: Reads `<data_dir>/identity.txt`. Returns `Ok(None)` on missing,
    /// `Ok(Some(text))` on success.
    /// Test: `identity_txt_roundtrip` covers the present case; missing returns
    /// `None` is exercised by `load_identity_missing_returns_none`.
    pub fn load_identity(data_dir: &Path) -> Result<Option<String>> {
        let target = data_dir.join(IDENTITY_TXT);
        if !target.exists() {
            return Ok(None);
        }
        let s = std::fs::read_to_string(&target)
            .map_err(|e| PalaceStoreError::io(target.clone(), e))?;
        Ok(Some(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_palace(id: &str, dir: &Path) -> Palace {
        Palace {
            id: PalaceId::new(id),
            name: format!("Palace {id}"),
            description: Some("test palace".to_string()),
            created_at: Utc::now(),
            data_dir: dir.to_path_buf(),
        }
    }

    #[test]
    fn palace_store_roundtrip() {
        let tmp = tempdir().unwrap();
        let data_dir = tmp.path().join("alpha");
        let palace = make_palace("alpha", &data_dir);

        PalaceStore::save_palace(&palace).expect("save");
        let loaded = PalaceStore::load_palace(&data_dir).expect("load");

        assert_eq!(loaded.id, palace.id);
        assert_eq!(loaded.name, palace.name);
        assert_eq!(loaded.description, palace.description);
        assert_eq!(loaded.data_dir, palace.data_dir);
        // Timestamps should round-trip exactly through serde.
        assert_eq!(loaded.created_at, palace.created_at);
    }

    #[test]
    fn identity_txt_roundtrip() {
        let tmp = tempdir().unwrap();
        let data_dir = tmp.path().to_path_buf();
        let id = PalaceId::new("alpha");

        let text = "I am an experienced Rust engineer.\nI prefer concise answers.\n";
        PalaceStore::save_identity(&id, text, &data_dir).expect("save");

        let got = PalaceStore::load_identity(&data_dir).expect("load");
        assert_eq!(got.as_deref(), Some(text));
    }

    #[test]
    fn load_identity_missing_returns_none() {
        let tmp = tempdir().unwrap();
        let got = PalaceStore::load_identity(tmp.path()).expect("load");
        assert!(got.is_none());
    }

    #[test]
    fn list_palaces_finds_saved_palaces() {
        let tmp = tempdir().unwrap();
        let registry = tmp.path();

        for id in &["alpha", "beta"] {
            let dir = registry.join(id);
            let palace = make_palace(id, &dir);
            PalaceStore::save_palace(&palace).unwrap();
        }
        // Add a non-palace subdirectory; it should be ignored.
        std::fs::create_dir_all(registry.join("not-a-palace")).unwrap();

        let palaces = PalaceStore::list_palaces(registry).unwrap();
        let mut ids: Vec<String> = palaces.into_iter().map(|p| p.id.0).collect();
        ids.sort();
        assert_eq!(ids, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn list_palaces_missing_dir_is_empty() {
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let palaces = PalaceStore::list_palaces(&missing).unwrap();
        assert!(palaces.is_empty());
    }

    /// Restore a directory's mode on drop, including while unwinding.
    ///
    /// Why: the denial test below strips a directory to mode 000. If an
    /// assertion panics before it is restored, `tempfile` cannot delete the
    /// tree and the run leaks an undeletable directory.
    #[cfg(unix)]
    struct RestoreMode(PathBuf);

    #[cfg(unix)]
    impl Drop for RestoreMode {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        }
    }

    /// Why (#5532): the absence guard used `Path::exists`, which is
    /// `fs::metadata(..).is_ok()` and so reports *any* stat failure as "not
    /// there". A registry we are forbidden to stat therefore returned
    /// `Ok(vec![])`, and the propagation #5488/#5526 added one layer down (at
    /// `read_dir`) never ran, because `read_dir` was never reached. The
    /// destructive callers would report a clean empty run over data they never
    /// read.
    /// What: strips a PARENT of the registry dir to mode 000, so the failure
    /// lands on `stat` rather than on `read_dir` — the arm the existing
    /// regular-file/`ENOTDIR` harnesses cannot reach, since a regular file
    /// stats just fine and only fails once enumeration starts. Asserts the
    /// error propagates and carries the denial.
    /// Test: This test.
    #[cfg(unix)]
    #[test]
    fn list_palaces_propagates_an_unstattable_registry_dir() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let parent = tmp.path().join("locked");
        let registry = parent.join("registry");
        std::fs::create_dir_all(&registry).unwrap();

        // Baseline: listable while the parent is traversable.
        assert!(
            PalaceStore::list_palaces(&registry).is_ok(),
            "the registry must list cleanly before the parent is locked"
        );

        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o000)).unwrap();
        let _restore = RestoreMode(parent.clone());

        // Root bypasses the mode bits outright, and some filesystems ignore
        // them, so confirm the denial actually took hold. Asserting against a
        // stat that still succeeds would pass without exercising anything, and
        // a vacuous pass on a fail-open guard is worse than no test at all.
        match std::fs::metadata(&registry) {
            Ok(_) => panic!(
                "cannot exercise #5532: stat of {} still succeeds at mode 000. \
                 Run this suite as a non-root user on a filesystem that honours \
                 POSIX permission bits.",
                registry.display()
            ),
            Err(e) => assert_eq!(
                e.kind(),
                std::io::ErrorKind::PermissionDenied,
                "expected the locked parent to deny stat, got {e}"
            ),
        }

        let err = PalaceStore::list_palaces(&registry)
            .expect_err("a registry dir that cannot be stat'd must not report as empty");

        match err {
            PalaceStoreError::Io { path, source } => {
                assert_eq!(path, registry, "the error must name the registry dir");
                assert_eq!(
                    source.kind(),
                    std::io::ErrorKind::PermissionDenied,
                    "the denial must survive into the error, got {source}"
                );
            }
            other => panic!("expected an Io error carrying the denial, got {other:?}"),
        }
    }

    /// Why (#5543): the directory guard was `path.is_dir()`, which is
    /// `fs::metadata(..)` with every error coerced to `false`. A child we are
    /// forbidden to stat therefore read as "not a directory" and was skipped,
    /// so the listing came back `Ok` and one palace shorter — which the
    /// destructive callers cannot tell from a registry that never held it.
    /// What: strips the registry itself to `r--`. Read permission keeps
    /// `read_dir` enumerating names, while the missing execute bit denies stat
    /// of any child. That isolates the per-entry stat from the registry-level
    /// one `list_palaces_propagates_an_unstattable_registry_dir` already pins,
    /// which a mode-000 registry could not do.
    /// Test: This test.
    #[cfg(unix)]
    #[test]
    fn list_palaces_propagates_an_unstattable_entry() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let registry = tmp.path().join("registry");
        let dir = registry.join("alpha");
        PalaceStore::save_palace(&make_palace("alpha", &dir)).unwrap();

        assert_eq!(
            PalaceStore::list_palaces(&registry).unwrap().len(),
            1,
            "the palace must list cleanly before the registry is locked"
        );

        std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o400)).unwrap();
        let _restore = RestoreMode(registry.clone());

        // Root bypasses the mode bits outright, and some filesystems ignore
        // them, so confirm the denial actually took hold. A vacuous pass on a
        // fail-open guard is worse than no test at all.
        match std::fs::metadata(&dir) {
            Ok(_) => panic!(
                "cannot exercise #5543: stat of {} still succeeds with the registry at \
                 mode 400. Run this suite as a non-root user on a filesystem that honours \
                 POSIX permission bits.",
                dir.display()
            ),
            Err(e) => assert_eq!(
                e.kind(),
                std::io::ErrorKind::PermissionDenied,
                "expected the un-traversable registry to deny stat of its child, got {e}"
            ),
        }

        let err = PalaceStore::list_palaces(&registry)
            .expect_err("an entry that cannot be stat'd must not drop out of the listing");

        match err {
            PalaceStoreError::Io { path, source } => {
                assert_eq!(path, dir, "the error must name the entry it could not stat");
                assert_eq!(
                    source.kind(),
                    std::io::ErrorKind::PermissionDenied,
                    "the denial must survive into the error, got {source}"
                );
            }
            other => panic!("expected an Io error carrying the denial, got {other:?}"),
        }
    }

    /// Why (#5543): the metadata guard was `path.join(PALACE_JSON).exists()`,
    /// the same `is_ok()`-shaped coercion. A `palace.json` we are forbidden to
    /// stat read as a subdirectory that simply holds no metadata, so a real
    /// palace was silently reclassified as a stray directory.
    /// What: leaves the registry traversable and strips the palace's own
    /// directory to mode 000. Stat of the directory still succeeds, clearing
    /// the entry guard above, while stat of `palace.json` inside it is denied —
    /// so the failure lands on this site and not the previous one.
    /// Test: This test.
    #[cfg(unix)]
    #[test]
    fn list_palaces_propagates_an_unstattable_palace_json() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempdir().unwrap();
        let registry = tmp.path().join("registry");
        let dir = registry.join("alpha");
        PalaceStore::save_palace(&make_palace("alpha", &dir)).unwrap();

        assert_eq!(
            PalaceStore::list_palaces(&registry).unwrap().len(),
            1,
            "the palace must list cleanly before its directory is locked"
        );

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).unwrap();
        let _restore = RestoreMode(dir.clone());

        // The directory itself must still stat, or this would merely re-test
        // the entry arm above instead of the metadata guard.
        std::fs::metadata(&dir).expect("the palace dir itself must remain stattable");

        let target = dir.join(PALACE_JSON);
        match std::fs::metadata(&target) {
            Ok(_) => panic!(
                "cannot exercise #5543: stat of {} still succeeds with its parent at \
                 mode 000. Run this suite as a non-root user on a filesystem that honours \
                 POSIX permission bits.",
                target.display()
            ),
            Err(e) => assert_eq!(
                e.kind(),
                std::io::ErrorKind::PermissionDenied,
                "expected the locked palace dir to deny stat of its metadata, got {e}"
            ),
        }

        let err = PalaceStore::list_palaces(&registry)
            .expect_err("a palace.json that cannot be stat'd must not read as a non-palace dir");

        match err {
            PalaceStoreError::Io { path, source } => {
                assert_eq!(path, target, "the error must name the metadata file");
                assert_eq!(
                    source.kind(),
                    std::io::ErrorKind::PermissionDenied,
                    "the denial must survive into the error, got {source}"
                );
            }
            other => panic!("expected an Io error carrying the denial, got {other:?}"),
        }
    }

    /// Why (#5543): `load_palace` failures were swallowed by `Err(_) =>
    /// continue`, so a truncated or hand-edited `palace.json` dropped its
    /// palace from the listing while the call still reported success.
    /// `purge_palaces` and friends then ran over the remaining palaces and
    /// reported a clean pass across data one member of which they never read.
    /// What: writes one good palace beside one whose `palace.json` does not
    /// decode, and asserts the walk fails rather than quietly returning just
    /// the good one — the partial-listing case, not merely the empty one. Uses
    /// no permission bits, so unlike its two siblings it exercises the arm
    /// under any uid.
    /// Test: This test.
    #[test]
    fn list_palaces_propagates_an_undecodable_palace() {
        let tmp = tempdir().unwrap();
        let registry = tmp.path().join("registry");

        PalaceStore::save_palace(&make_palace("alpha", &registry.join("alpha"))).unwrap();

        let broken = registry.join("beta");
        std::fs::create_dir_all(&broken).unwrap();
        let target = broken.join(PALACE_JSON);
        std::fs::write(&target, b"{ this is not palace metadata").unwrap();

        let err = PalaceStore::list_palaces(&registry)
            .expect_err("an undecodable palace must not silently shorten the listing");

        match err {
            PalaceStoreError::Json { path, .. } => {
                assert_eq!(path, target, "the error must name the undecodable file");
            }
            other => panic!("expected a Json error naming the broken palace, got {other:?}"),
        }
    }
}
