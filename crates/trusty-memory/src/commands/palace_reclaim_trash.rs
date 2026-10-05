//! The trash side of `palace reclaim` (#9140 ruling f0): root sanity, the
//! dated trash dir, its `manifest.json`, store-lock probing, and the 7-day
//! purge.
//!
//! Why: the owner ruled that reclaimed items go to a trash dir, not straight to
//! deletion, and that the trash is purged after 7 days. Every guard that keeps
//! the apply and the purge inside `<root>/.trash/` lives here, so one file
//! holds the rules a reviewer must check.
//! What: [`check_root`] refuses `/`, a child of `/`, `$HOME` and its
//! ancestors. [`prepare_trash_dir`] makes `<root>/.trash/<date>-reclaim/` on
//! the root's own volume. [`lock_stores`] takes an exclusive `flock` on every
//! redb store in a directory, or reports the one the live daemon holds.
//! [`purge_trash`] removes only dated, manifest-bearing, real directories
//! directly under `<root>/.trash/` that are more than 7 days old.
//! Test: `reclaim_root_check_refuses_root_home_and_its_ancestors`,
//! `purge_removes_only_old_dated_trash_dirs_with_a_manifest`,
//! `purge_refuses_a_symlinked_trash_base` (in `palace_reclaim_apply_tests.rs`).

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use super::palace_reclaim::ReclaimClass;

/// Hidden dir under the palace root that holds every reclaim trash dir. The
/// dry-run scan skips hidden entries, so the trash is never re-listed.
pub(crate) const TRASH_DIR: &str = ".trash";

/// Suffix of a dated trash dir: `<YYYY-MM-DD>-reclaim`.
pub(crate) const TRASH_SUFFIX: &str = "-reclaim";

/// The record of what one trash dir holds.
pub(crate) const MANIFEST_FILE: &str = "manifest.json";

/// A trash dir is purged once its date is more than this many days old.
pub(crate) const TRASH_RETENTION_DAYS: i64 = 7;

/// What happened to one reviewed item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    /// Matched; the move has not run yet. A manifest left in this state by a
    /// crash still names every item the run may have moved.
    Pending,
    /// Renamed into the trash dir.
    Moved,
    /// Inside a directory item that was renamed into the trash dir.
    MovedWithParent,
    /// Exported to the trash dir, then removed through the daemon.
    DrawerRemoved,
    /// Left in place: changed, held by the live daemon, or its parent stayed.
    Skipped,
    /// Left in place after an error.
    Failed,
}

/// One manifest row: an item, where it went, and why.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrashEntry {
    pub original_path: PathBuf,
    /// Where the item is in the trash; for a drawer, its JSON export.
    pub trash_path: PathBuf,
    pub class: ReclaimClass,
    pub bytes: u64,
    pub reason: String,
    pub palace: Option<String>,
    pub drawer_id: Option<String>,
    pub status: EntryStatus,
    /// The skip or failure reason, when there is one.
    pub detail: Option<String>,
}

/// `manifest.json`: every item one or more same-day runs touched.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TrashManifest {
    pub root: PathBuf,
    /// The reviewed list each run consumed, in run order.
    pub reviewed_lists: Vec<PathBuf>,
    pub entries: Vec<TrashEntry>,
}

/// Refuse a reclaim root that is `/`, a direct child of `/`, `home`, or an
/// ancestor of `home` (#9140 ruling f0).
///
/// Why: the apply renames and the purge deletes whole directories, so a root
/// resolved from a degenerate environment must stop the run, not narrow it.
/// `sanitize_data_root` repairs such a path for the daemon; a destructive
/// command refuses instead.
/// What: `root` must be absolute and canonicalize; the canonical path must
/// not be `/` or its child, and canonical `home` must not lie inside it. An
/// unknown `home` is a refusal. Returns the canonical root.
/// Test: `reclaim_root_check_refuses_root_home_and_its_ancestors`.
pub(crate) fn check_root(root: &Path, home: Option<&Path>) -> Result<PathBuf> {
    if !root.is_absolute() {
        bail!(
            "reclaim root {} is not absolute; refusing (#9140)",
            root.display()
        );
    }
    let canon = std::fs::canonicalize(root)
        .with_context(|| format!("canonicalize reclaim root {}", root.display()))?;
    if canon.parent().is_none_or(|p| p == Path::new("/")) {
        bail!(
            "reclaim root {} is / or a direct child of /; refusing (#9140)",
            canon.display()
        );
    }
    let Some(home) = home else {
        bail!("cannot resolve the home directory to vet the reclaim root; refusing (#9140)");
    };
    let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
    if home.starts_with(&canon) {
        bail!(
            "reclaim root {} is the home directory or an ancestor of it; refusing (#9140)",
            canon.display()
        );
    }
    Ok(canon)
}

/// Make `<root>/.trash/<date>-reclaim/` and prove it is on `root`'s volume.
///
/// Why: a move into the trash must be a `rename`, never a copy; a trash dir on
/// another volume would make every rename fail, and a symlinked one would put
/// the trash outside the root.
/// What: creates each level that is absent; refuses a level that is a symlink
/// or not a directory, and a dated dir whose device differs from `root`'s.
pub(crate) fn prepare_trash_dir(root: &Path, date: &str) -> Result<PathBuf> {
    let base = root.join(TRASH_DIR);
    ensure_real_dir(&base)?;
    let dated = base.join(format!("{date}{TRASH_SUFFIX}"));
    ensure_real_dir(&dated)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let root_dev = std::fs::metadata(root)?.dev();
        let trash_dev = std::fs::symlink_metadata(&dated)?.dev();
        if root_dev != trash_dev {
            bail!(
                "trash dir {} is on another volume than {}; a move would copy, refusing (#9140)",
                dated.display(),
                root.display()
            );
        }
    }
    Ok(dated)
}

/// Create `dir` when absent; refuse a symlink or a non-directory.
fn ensure_real_dir(dir: &Path) -> Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => bail!(
            "{} exists but is not a real directory (a symlink or a file); refusing (#9140)",
            dir.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(dir).with_context(|| format!("create trash dir {}", dir.display()))
        }
        Err(e) => Err(e).with_context(|| format!("stat trash dir {}", dir.display())),
    }
}

/// Read `<trash>/manifest.json`, or an empty manifest for a new trash dir.
pub(crate) fn read_manifest(trash: &Path, root: &Path) -> Result<TrashManifest> {
    let path = trash.join(MANIFEST_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parse existing trash manifest {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TrashManifest {
            root: root.to_path_buf(),
            ..Default::default()
        }),
        Err(e) => Err(e).with_context(|| format!("read trash manifest {}", path.display())),
    }
}

/// Write `<trash>/manifest.json` through a temp file and a rename.
pub(crate) fn write_manifest(trash: &Path, manifest: &TrashManifest) -> Result<()> {
    let path = trash.join(MANIFEST_FILE);
    let tmp = trash.join(format!("{MANIFEST_FILE}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(manifest)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("install {}", path.display()))
}

/// Exclusive locks on every redb store under a directory, or the store the
/// live daemon holds.
pub(crate) enum StoreLocks {
    /// Every store is locked by this process; drop to release.
    Acquired(Vec<File>),
    /// This store's `flock` is held elsewhere — the live daemon has it open.
    Held(PathBuf),
}

/// Take an exclusive, non-blocking `flock` on every `*.redb` file under `dir`.
///
/// Why (#9140 ruling f0): a palace the live daemon holds open is never moved
/// or modified. redb takes the same `flock` on open (exclusive for a writer,
/// shared for a reader), so a lock this process cannot take is a store the
/// daemon has open. Holding the locks across the rename also stops the daemon
/// opening the store mid-move: its open fails `DatabaseAlreadyOpen` instead.
/// What: walks `dir` without following symlinks; opens each regular
/// `*.redb` file read-only and `try_lock`s it. The first `WouldBlock` is
/// [`StoreLocks::Held`]; any other error is an `Err`, never a pass.
/// Test: `apply_skips_a_palace_whose_store_is_locked`.
pub(crate) fn lock_stores(dir: &Path) -> Result<StoreLocks> {
    let mut locks = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&d)
            .with_context(|| format!("list {}", d.display()))?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<_>>()
            .with_context(|| format!("list {}", d.display()))?;
        entries.sort();
        for path in entries {
            let meta = std::fs::symlink_metadata(&path)
                .with_context(|| format!("stat {}", path.display()))?;
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() || path.extension().is_none_or(|x| x != "redb") {
                continue;
            }
            let file = File::open(&path).with_context(|| format!("open {}", path.display()))?;
            match file.try_lock() {
                Ok(()) => locks.push(file),
                Err(TryLockError::WouldBlock) => return Ok(StoreLocks::Held(path)),
                Err(TryLockError::Error(e)) => {
                    return Err(e).with_context(|| format!("flock {}", path.display()));
                }
            }
        }
    }
    Ok(StoreLocks::Acquired(locks))
}

/// What `--purge-trash` removed and what it left, with the reason.
#[derive(Debug, Default, Serialize)]
pub struct PurgeReport {
    pub removed: Vec<PathBuf>,
    pub kept: Vec<(PathBuf, String)>,
    /// `(path, error)` for a trash dir whose removal failed.
    pub failed: Vec<(PathBuf, String)>,
}

/// Remove trash dirs more than [`TRASH_RETENTION_DAYS`] old (#9140 ruling f0).
///
/// Why: the purge is the one place reclaim deletes, so it deletes only what
/// the apply made: a real directory directly under `<root>/.trash/`, named
/// `<YYYY-MM-DD>-reclaim`, holding a regular `manifest.json`.
/// What: a missing `.trash` is an empty report; a `.trash` that is a symlink
/// or a file is an error. Each entry failing a rule is `kept` with the rule.
/// Removal uses `remove_dir_all`, which unlinks symlinks inside the dir
/// rather than following them.
/// Test: `purge_removes_only_old_dated_trash_dirs_with_a_manifest`,
/// `purge_refuses_a_symlinked_trash_base`.
pub(crate) fn purge_trash(root: &Path, today: NaiveDate) -> Result<PurgeReport> {
    let mut report = PurgeReport::default();
    let base = root.join(TRASH_DIR);
    match std::fs::symlink_metadata(&base) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => bail!(
            "{} is not a real directory (a symlink or a file); refusing to purge (#9140)",
            base.display()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(e) => return Err(e).with_context(|| format!("stat {}", base.display())),
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&base)
        .with_context(|| format!("list {}", base.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for path in entries {
        match purge_verdict(&path, today) {
            Ok(()) => match std::fs::remove_dir_all(&path) {
                Ok(()) => report.removed.push(path),
                Err(e) => report.failed.push((path, e.to_string())),
            },
            Err(why) => report.kept.push((path, why)),
        }
    }
    Ok(report)
}

/// `Ok` when `path` is a trash dir the purge may remove, else the rule it fails.
fn purge_verdict(path: &Path, today: NaiveDate) -> std::result::Result<(), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("stat: {e}"))?;
    if !meta.is_dir() {
        return Err("not a real directory (a symlink or a file)".into());
    }
    let date = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(TRASH_SUFFIX))
        .filter(|d| d.len() == 10)
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .ok_or_else(|| format!("name is not <YYYY-MM-DD>{TRASH_SUFFIX}"))?;
    let manifest = std::fs::symlink_metadata(path.join(MANIFEST_FILE));
    if !manifest.is_ok_and(|m| m.is_file()) {
        return Err(format!("holds no regular {MANIFEST_FILE}"));
    }
    let age = (today - date).num_days();
    if age <= TRASH_RETENTION_DAYS {
        return Err(format!(
            "{age} days old; kept until older than {TRASH_RETENTION_DAYS}"
        ));
    }
    Ok(())
}
