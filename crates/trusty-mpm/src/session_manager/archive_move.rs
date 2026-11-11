//! Move a directory into the privileged archive with a manifest (#10991).
//!
//! Why: owner ruling D8b — archive moves a session's project directory to
//! `~/.trusty-tools/archive`, an owner-only (0700) root, and the move must be
//! reversible. A manifest that records the origin path is what makes a
//! restore possible, so a move without one is a failure, not a success.
//! What: [`plan_archive`] validates the source and names the archive entry
//! without touching disk; [`execute_plan`] creates the root and entry at 0700,
//! moves the directory into `<entry>/content` by `rename(2)`, falls back to a
//! copy that must verify byte-for-byte when the rename crosses filesystems,
//! then writes `<entry>/manifest.json`. A failed manifest write undoes the
//! move; a copy that does not verify is deleted and the source is kept.
//! Test: `archive_move_tests.rs`.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Mode of the archive root and of every entry under it (owner ruling D8b).
pub const ARCHIVE_DIR_MODE: u32 = 0o700;
/// The manifest's file name inside an archive entry.
pub const MANIFEST_FILE: &str = "manifest.json";
/// The moved directory's name inside an archive entry.
pub const CONTENT_DIR: &str = "content";
/// The manifest schema version this build writes.
pub const MANIFEST_VERSION: u32 = 1;

/// Why an archive move did not complete (#10991).
#[derive(Debug, Error)]
pub enum ArchiveMoveError {
    /// The source is missing or is not a directory.
    #[error("archive source {0} is not a directory")]
    NotADirectory(PathBuf),
    /// The source is a path archive must never move.
    #[error("refusing to archive {path}: {reason}")]
    Refused {
        /// The refused source.
        path: PathBuf,
        /// Why it is refused.
        reason: String,
    },
    /// An entry with the planned name already exists; nothing is overwritten.
    #[error("archive entry {0} already exists")]
    EntryExists(PathBuf),
    /// The cross-filesystem copy did not match the source; the copy was
    /// removed and the source kept.
    #[error("archive copy of {path} did not verify ({reason}); the source was kept")]
    VerifyFailed {
        /// The source directory.
        path: PathBuf,
        /// The first mismatch found.
        reason: String,
    },
    /// The manifest could not be written; the move was undone.
    #[error("archive manifest {path} could not be written ({reason}); {rollback}")]
    Manifest {
        /// The manifest path.
        path: PathBuf,
        /// The write error.
        reason: String,
        /// What the rollback did.
        rollback: String,
    },
    /// The verified copy and its manifest exist, but the source could not be
    /// removed; both trees are on disk.
    #[error("archived {path} to {entry}, but the source could not be removed: {reason}")]
    SourceKept {
        /// The source directory, still present.
        path: PathBuf,
        /// The complete archive entry.
        entry: PathBuf,
        /// The removal error.
        reason: String,
    },
    /// Any other filesystem error, with what was being done.
    #[error("{context}: {error}")]
    Io {
        /// What was being done.
        context: String,
        /// The error.
        error: io::Error,
    },
    /// No home directory to place the archive root under.
    #[error("no home directory to place the archive root under")]
    NoHome,
}

/// How the directory reached the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveMethod {
    /// A same-filesystem `rename(2)`.
    Rename,
    /// A cross-filesystem copy that verified, then removal of the source.
    CopyVerified,
}

/// The record that makes an archive entry restorable (#10991).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveManifest {
    /// Schema version ([`MANIFEST_VERSION`]).
    pub version: u32,
    /// Where the directory lived; a restore moves `archived_path` back here.
    pub origin_path: PathBuf,
    /// Where the directory lives now (`<entry>/content`).
    pub archived_path: PathBuf,
    /// When the move completed.
    pub archived_at: DateTime<Utc>,
    /// The session whose directory this is.
    pub session_id: String,
    /// The registry project the session resolved to, if any.
    pub project: Option<String>,
    /// How the directory moved.
    pub method: MoveMethod,
}

/// A validated archive move, computed without touching disk (#10991).
///
/// The dry run returns this; the real run executes the same value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchivePlan {
    /// The canonical source directory.
    pub source: PathBuf,
    /// The archive root the entry sits in.
    pub root: PathBuf,
    /// The archive entry directory.
    pub entry: PathBuf,
    /// Where the source will live (`<entry>/content`).
    pub destination: PathBuf,
    /// Where the manifest will be written.
    pub manifest_path: PathBuf,
    /// The session whose directory this is.
    pub session_id: String,
    /// The registry project the session resolved to, if any.
    pub project: Option<String>,
}

fn io_err(context: impl Into<String>) -> impl FnOnce(io::Error) -> ArchiveMoveError {
    let context = context.into();
    move |error| ArchiveMoveError::Io { context, error }
}

/// `~/.trusty-tools/archive` (owner ruling D8b).
pub fn default_archive_root() -> Result<PathBuf, ArchiveMoveError> {
    dirs::home_dir()
        .map(|h| h.join(".trusty-tools").join("archive"))
        .ok_or(ArchiveMoveError::NoHome)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// Create `root` if absent and set it to [`ARCHIVE_DIR_MODE`].
///
/// Why: D8b's "privileged" means owner-only, and a root that already exists
/// with a looser mode is tightened rather than trusted. A symlinked root is
/// refused: its target's mode is not ours to vouch for.
/// Test: `archive_root_is_created_owner_only`,
/// `an_existing_loose_archive_root_is_tightened`,
/// `a_symlinked_archive_root_is_refused`.
pub fn ensure_archive_root(root: &Path) -> Result<(), ArchiveMoveError> {
    if let Ok(meta) = fs::symlink_metadata(root)
        && meta.file_type().is_symlink()
    {
        return Err(ArchiveMoveError::Refused {
            path: root.to_path_buf(),
            reason: "the archive root is a symlink".into(),
        });
    }
    fs::create_dir_all(root).map_err(io_err(format!("create {}", root.display())))?;
    set_mode(root, ARCHIVE_DIR_MODE).map_err(io_err(format!("chmod {}", root.display())))
}

/// `path` canonicalized; when it does not exist yet, its nearest existing
/// ancestor canonicalized with the missing tail re-joined, so a symlinked
/// parent (macOS `/var` -> `/private/var`) cannot defeat a containment check.
fn canonical_or_nearest(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut cur = path;
    loop {
        if let Ok(c) = fs::canonicalize(cur) {
            return tail.iter().rev().fold(c, |acc, part| acc.join(part));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                tail.push(name.to_os_string());
                cur = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// Validate `source` and name its archive entry under `root`; no disk write.
///
/// Why: the dry run and the real run must agree, so both start here.
/// What: canonicalizes `source` (missing or not a directory is
/// [`ArchiveMoveError::NotADirectory`]); refuses `/`, the home directory or
/// any ancestor of it, a source already inside `root`, and a source that
/// contains `root`. The entry is `<root>/<leaf>-<UTC stamp>-<id prefix>`; an
/// existing entry is [`ArchiveMoveError::EntryExists`].
/// Test: `plan_refuses_home_and_the_archive_itself`,
/// `plan_refuses_a_missing_source`.
pub fn plan_archive(
    source: &Path,
    root: &Path,
    session_id: &str,
    project: Option<String>,
    at: DateTime<Utc>,
) -> Result<ArchivePlan, ArchiveMoveError> {
    let source =
        fs::canonicalize(source).map_err(|_| ArchiveMoveError::NotADirectory(source.into()))?;
    if !source.is_dir() {
        return Err(ArchiveMoveError::NotADirectory(source));
    }
    let refuse = |reason: &str| ArchiveMoveError::Refused {
        path: source.clone(),
        reason: reason.into(),
    };
    if source.parent().is_none() {
        return Err(refuse("it is the filesystem root"));
    }
    if let Some(home) = dirs::home_dir().and_then(|h| fs::canonicalize(h).ok())
        && home.starts_with(&source)
    {
        return Err(refuse("it is the home directory or contains it"));
    }
    let root_abs = canonical_or_nearest(root);
    if source.starts_with(&root_abs) {
        return Err(refuse("it is already inside the archive"));
    }
    if root_abs.starts_with(&source) {
        return Err(refuse("the archive root is inside it"));
    }
    let leaf: String = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let short: String = session_id.chars().take(8).collect();
    let entry = root_abs.join(format!("{leaf}-{}-{short}", at.format("%Y%m%dT%H%M%SZ")));
    if fs::symlink_metadata(&entry).is_ok() {
        return Err(ArchiveMoveError::EntryExists(entry));
    }
    Ok(ArchivePlan {
        destination: entry.join(CONTENT_DIR),
        manifest_path: entry.join(MANIFEST_FILE),
        entry,
        root: root_abs,
        source,
        session_id: session_id.to_string(),
        project,
    })
}

/// Fault-injection points for tests; production passes [`Hooks::NONE`].
pub(crate) struct Hooks<'a> {
    /// Runs after a copy, before it is verified.
    pub after_copy: &'a dyn Fn(&Path),
    /// Runs before the manifest is written.
    pub before_manifest: &'a dyn Fn(&Path),
}

impl Hooks<'_> {
    pub(crate) const NONE: Hooks<'static> = Hooks {
        after_copy: &|_| {},
        before_manifest: &|_| {},
    };
}

/// Whether to try `rename(2)` first, or go straight to copy-and-verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Strategy {
    /// Rename; copy only when the rename crosses filesystems.
    Auto,
    /// Copy, as a cross-filesystem move would (tests).
    ForceCopy,
}

/// Execute `plan`: move the source into the archive and write its manifest.
///
/// Why: see the module doc. What: [`execute_with`] with [`Strategy::Auto`].
/// Test: `rename_moves_the_directory_and_writes_the_manifest`.
pub fn execute_plan(plan: &ArchivePlan) -> Result<ArchiveManifest, ArchiveMoveError> {
    execute_with(plan, Strategy::Auto, &Hooks::NONE)
}

/// [`execute_plan`] with an explicit strategy and fault hooks.
///
/// Test: `copy_moves_the_directory_and_verifies_it`,
/// `a_copy_that_does_not_verify_keeps_the_source`,
/// `a_failed_manifest_write_undoes_a_rename`,
/// `a_failed_manifest_write_removes_a_copy`,
/// `a_source_that_cannot_be_removed_is_reported`.
pub(crate) fn execute_with(
    plan: &ArchivePlan,
    strategy: Strategy,
    hooks: &Hooks<'_>,
) -> Result<ArchiveManifest, ArchiveMoveError> {
    ensure_archive_root(&plan.root)?;
    match fs::create_dir(&plan.entry) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            return Err(ArchiveMoveError::EntryExists(plan.entry.clone()));
        }
        Err(e) => return Err(io_err(format!("create {}", plan.entry.display()))(e)),
    }
    if let Err(e) = set_mode(&plan.entry, ARCHIVE_DIR_MODE) {
        let _ = fs::remove_dir(&plan.entry);
        return Err(io_err(format!("chmod {}", plan.entry.display()))(e));
    }
    let method = match move_into_entry(plan, strategy, hooks) {
        Ok(m) => m,
        Err(e) => {
            // The source is untouched; drop the empty or partial entry.
            let _ = fs::remove_dir_all(&plan.entry);
            return Err(e);
        }
    };
    let manifest = ArchiveManifest {
        version: MANIFEST_VERSION,
        origin_path: plan.source.clone(),
        archived_path: plan.destination.clone(),
        archived_at: Utc::now(),
        session_id: plan.session_id.clone(),
        project: plan.project.clone(),
        method,
    };
    (hooks.before_manifest)(&plan.manifest_path);
    // #10991 fail-open check: an entry without a manifest is never left behind.
    if let Err(reason) = write_manifest(&plan.manifest_path, &manifest) {
        let rollback = undo_move(plan, method);
        return Err(ArchiveMoveError::Manifest {
            path: plan.manifest_path.clone(),
            reason,
            rollback,
        });
    }
    if method == MoveMethod::CopyVerified
        && let Err(e) = fs::remove_dir_all(&plan.source)
    {
        return Err(ArchiveMoveError::SourceKept {
            path: plan.source.clone(),
            entry: plan.entry.clone(),
            reason: e.to_string(),
        });
    }
    Ok(manifest)
}

fn move_into_entry(
    plan: &ArchivePlan,
    strategy: Strategy,
    hooks: &Hooks<'_>,
) -> Result<MoveMethod, ArchiveMoveError> {
    if strategy == Strategy::Auto {
        match fs::rename(&plan.source, &plan.destination) {
            Ok(()) => return Ok(MoveMethod::Rename),
            Err(e) if is_cross_device(&e) => {}
            Err(e) => {
                return Err(io_err(format!(
                    "rename {} to {}",
                    plan.source.display(),
                    plan.destination.display()
                ))(e));
            }
        }
    }
    copy_tree(&plan.source, &plan.destination)?;
    (hooks.after_copy)(&plan.destination);
    verify_copy(&plan.source, &plan.destination).map_err(|reason| {
        ArchiveMoveError::VerifyFailed {
            path: plan.source.clone(),
            reason,
        }
    })?;
    Ok(MoveMethod::CopyVerified)
}

fn is_cross_device(e: &io::Error) -> bool {
    #[cfg(unix)]
    let exdev = e.raw_os_error() == Some(libc::EXDEV);
    #[cfg(not(unix))]
    let exdev = false;
    e.kind() == io::ErrorKind::CrossesDevices || exdev
}

/// Put the source back after a failed manifest write; describe the outcome.
fn undo_move(plan: &ArchivePlan, method: MoveMethod) -> String {
    let undone = match method {
        MoveMethod::Rename => fs::rename(&plan.destination, &plan.source)
            .map(|()| format!("the directory was moved back to {}", plan.source.display())),
        // The source was never removed; only the copy goes.
        MoveMethod::CopyVerified => Ok(format!(
            "the copy was removed and {} kept",
            plan.source.display()
        )),
    };
    match undone {
        Ok(msg) => {
            let _ = fs::remove_dir_all(&plan.entry);
            msg
        }
        Err(e) => format!(
            "ROLLBACK FAILED, the directory is at {}: {e}",
            plan.destination.display()
        ),
    }
}

fn write_manifest(path: &Path, manifest: &ArchiveManifest) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(manifest).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    let written = fs::write(&tmp, body)
        .and_then(|()| set_mode(&tmp, 0o600))
        .and_then(|()| fs::rename(&tmp, path));
    written.map_err(|e| {
        let _ = fs::remove_file(&tmp);
        e.to_string()
    })
}

/// Read an entry's manifest.
///
/// Test: `rename_moves_the_directory_and_writes_the_manifest`.
pub fn read_manifest(entry: &Path) -> Result<ArchiveManifest, ArchiveMoveError> {
    let path = entry.join(MANIFEST_FILE);
    let body = fs::read(&path).map_err(io_err(format!("read {}", path.display())))?;
    serde_json::from_slice(&body).map_err(|e| ArchiveMoveError::Io {
        context: format!("parse {}", path.display()),
        error: io::Error::new(io::ErrorKind::InvalidData, e),
    })
}

/// Copy `src` to `dst` (which must not exist): directories, regular files and
/// symlinks, with their modes. Any other file type is refused.
fn copy_tree(src: &Path, dst: &Path) -> Result<(), ArchiveMoveError> {
    let mut dirs_seen = Vec::new();
    for entry in walkdir::WalkDir::new(src).follow_links(false) {
        let entry = entry.map_err(|e| ArchiveMoveError::Io {
            context: format!("walk {}", src.display()),
            error: e.into(),
        })?;
        let rel = entry.path().strip_prefix(src).unwrap_or(entry.path());
        let target = dst.join(rel);
        let ft = entry.file_type();
        if ft.is_dir() {
            fs::create_dir(&target).map_err(io_err(format!("mkdir {}", target.display())))?;
            dirs_seen.push((entry.path().to_path_buf(), target));
        } else if ft.is_file() {
            fs::copy(entry.path(), &target)
                .map_err(io_err(format!("copy {}", entry.path().display())))?;
        } else if ft.is_symlink() {
            let link = fs::read_link(entry.path())
                .map_err(io_err(format!("readlink {}", entry.path().display())))?;
            make_symlink(&link, &target)
                .map_err(io_err(format!("symlink {}", target.display())))?;
        } else {
            return Err(ArchiveMoveError::Refused {
                path: entry.path().to_path_buf(),
                reason: "a special file (fifo, socket or device) cannot be copied".into(),
            });
        }
    }
    // Modes last, deepest first, so a read-only directory still got its children.
    for (from, to) in dirs_seen.iter().rev() {
        let perms = fs::metadata(from)
            .map_err(io_err(format!("stat {}", from.display())))?
            .permissions();
        fs::set_permissions(to, perms).map_err(io_err(format!("chmod {}", to.display())))?;
    }
    Ok(())
}

#[cfg(unix)]
fn make_symlink(link: &Path, at: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(link, at)
}

#[cfg(not(unix))]
fn make_symlink(_link: &Path, _at: &Path) -> io::Result<()> {
    Err(io::Error::other("symlinks are not copied on this platform"))
}

/// Check `dst` is the same tree as `src`: the same relative paths, file types,
/// symlink targets and file bytes. Returns the first mismatch.
///
/// Test: `a_copy_that_does_not_verify_keeps_the_source`.
pub(crate) fn verify_copy(src: &Path, dst: &Path) -> Result<(), String> {
    let list = |root: &Path| -> Result<Vec<PathBuf>, String> {
        let mut out = Vec::new();
        for e in walkdir::WalkDir::new(root)
            .follow_links(false)
            .sort_by_file_name()
        {
            let e = e.map_err(|e| e.to_string())?;
            out.push(
                e.path()
                    .strip_prefix(root)
                    .unwrap_or(e.path())
                    .to_path_buf(),
            );
        }
        Ok(out)
    };
    let (a, b) = (list(src)?, list(dst)?);
    if a != b {
        return Err(format!(
            "the trees differ: {} entries in the source, {} in the copy",
            a.len(),
            b.len()
        ));
    }
    for rel in &a {
        let (s, d) = (src.join(rel), dst.join(rel));
        let (sm, dm) = (
            fs::symlink_metadata(&s).map_err(|e| e.to_string())?,
            fs::symlink_metadata(&d).map_err(|e| e.to_string())?,
        );
        let (sf, df) = (sm.file_type(), dm.file_type());
        if sf.is_dir() != df.is_dir() || sf.is_symlink() != df.is_symlink() {
            return Err(format!("{} changed type", rel.display()));
        }
        if sf.is_symlink() {
            if fs::read_link(&s).ok() != fs::read_link(&d).ok() {
                return Err(format!("{} points elsewhere", rel.display()));
            }
        } else if sf.is_file() && (sm.len() != dm.len() || !same_bytes(&s, &d)?) {
            return Err(format!("{} differs", rel.display()));
        }
    }
    Ok(())
}

fn same_bytes(a: &Path, b: &Path) -> Result<bool, String> {
    let open = |p: &Path| fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()));
    let (mut fa, mut fb) = (open(a)?, open(b)?);
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let na = read_full(&mut fa, &mut ba)?;
        let nb = read_full(&mut fb, &mut bb)?;
        if na != nb || ba[..na] != bb[..nb] {
            return Ok(false);
        }
        if na == 0 {
            return Ok(true);
        }
    }
}

fn read_full(f: &mut fs::File, buf: &mut [u8]) -> Result<usize, String> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(n)
}

#[cfg(all(test, unix))]
#[path = "archive_move_tests.rs"]
mod tests;
