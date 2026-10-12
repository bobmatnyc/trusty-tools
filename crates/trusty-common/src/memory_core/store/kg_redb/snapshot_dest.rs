//! Where a manual palace backup may be written (#9282, option C).
//!
//! Why: a backup destination is user input from an MCP client. A path that
//! lands in the palace registry dir is listed as a palace, and one that
//! creates `<data root>/palaces` switches the daemon's registry on the next
//! start, so every palace vanishes. Checking where a not-yet-existing path
//! *would* resolve kept missing cases (`..` after a missing segment, a
//! dangling symlink, `palaceſ` folding to `palaces` on APFS).
//! What: a backup goes only under `<backup data root>/backups/manual/`
//! (ADR-0067 D3 rule 3; ADR-0066 D7). The name is a relative path of plain
//! segments ([`check_backup_name`], no I/O). [`manual_backup_root`] makes or
//! verifies the root with no symlinked segment; [`prepare_dest`] verifies or
//! creates each parent with `lstat` + `mkdir`, which never follow a symlink
//! at the last component, then re-checks the canonical parent against the
//! root. Every created path is the canonical root joined with plain names.
//! Residual risk (ruling D): a same-user writer swapping a segment for a
//! symlink between the walk and the re-check can make at most one empty
//! directory outside the root, and the re-check then refuses the backup;
//! a swap after the re-check and before the final rename can redirect it.
//! Test: `snapshot_dest_tests.rs`.

use super::io_util::io_err;
use super::{Result, SnapshotError, SnapshotHook, SnapshotStep};
use std::io;
use std::path::{Path, PathBuf};

/// Fixed segments under the flat-layout registry and under the legacy data root.
const FLAT_SEGMENTS: [&str; 3] = [".trusty-memory", "backups", "manual"];
const LEGACY_SEGMENTS: [&str; 2] = ["backups", "manual"];
/// How often a segment that vanished between `mkdir`'s EEXIST and the
/// re-`lstat` is retried before the walk gives up.
const RACE_RETRIES: usize = 3;

/// Refuse a backup name that is not a relative path of plain segments.
///
/// Why: the name is joined to the backup root; an absolute path, `..` or a
/// `.` would point the join somewhere else.
/// What: pure, no I/O. Reads the raw bytes, because `Path::components`
/// drops an interior `.` and a trailing `/`. Splits on `/` (and `\` on
/// Windows) and refuses an empty name, a NUL byte, an absolute path (a
/// leading separator, or a drive or root prefix on Windows), an empty
/// segment, `.` and `..`, each as [`SnapshotError::InvalidBackupName`].
/// Test: `backup_name_rules_accept_plain_segments_only`,
/// `an_absolute_dest_is_refused_and_writes_nothing`.
pub fn check_backup_name(name: &Path) -> Result<()> {
    let refuse = |reason: &str| {
        Err(SnapshotError::InvalidBackupName {
            name: name.to_path_buf(),
            reason: reason.to_string(),
        })
    };
    let bytes = name.as_os_str().as_encoded_bytes();
    let is_sep = |b: &u8| *b == b'/' || (cfg!(windows) && *b == b'\\');
    if bytes.is_empty() {
        return refuse("it is empty");
    }
    if bytes.contains(&0) {
        return refuse("it contains a NUL byte");
    }
    let prefixed = name.components().any(|c| {
        matches!(
            c,
            std::path::Component::Prefix(_) | std::path::Component::RootDir
        )
    });
    if bytes.first().is_some_and(is_sep) || name.is_absolute() || prefixed {
        return refuse("it is an absolute path, or has a drive or root prefix");
    }
    for segment in bytes.split(is_sep) {
        match segment {
            b"" => return refuse("it has an empty segment ('//' or a trailing '/')"),
            b"." => return refuse("it has a '.' segment"),
            b".." => return refuse("it has a '..' segment"),
            _ => {}
        }
    }
    Ok(())
}

/// Make or verify `<backup data root>/backups/manual` for `palace_dir`.
///
/// Why: the root's fixed segments must be real directories; a symlinked
/// `backups -> palaces` would carry every backup into the registry.
/// What: canonicalises the registry (`palace_dir`'s parent). When its name
/// is `palaces` the base is its parent and the segments are
/// `backups/manual`; otherwise the base is the registry and the segments are
/// `.trusty-memory/backups/manual` (the `backup_data_root` rule). Each
/// segment is `lstat`ed: missing is created, a real directory is accepted,
/// a symlink or non-directory is [`SnapshotError::BackupRootUnsafe`], any
/// other error is `Io`. A palace dir with no parent is `Io { op: "resolve" }`.
/// Segments created before a later refusal stay; they are the empty root.
/// Test: `a_symlinked_backup_root_segment_is_refused`,
/// `a_backup_root_segment_that_is_a_file_is_refused`,
/// `an_unreadable_backup_root_segment_is_an_error_and_writes_nothing`,
/// `a_palace_dir_without_a_parent_is_refused`,
/// `an_uncanonicalisable_registry_is_an_error`.
pub(crate) fn manual_backup_root(palace_dir: &Path) -> Result<PathBuf> {
    let palace_dir = std::path::absolute(palace_dir).map_err(io_err("resolve", palace_dir))?;
    // #9282: fail closed; a palace dir with no parent has no backup root.
    let registry = palace_dir.parent().ok_or_else(|| SnapshotError::Io {
        op: "resolve",
        path: palace_dir.clone(),
        source: io::Error::new(
            io::ErrorKind::InvalidInput,
            "the palace directory has no parent registry directory",
        ),
    })?;
    let registry = registry
        .canonicalize()
        .map_err(io_err("resolve", registry))?;
    let (mut root, segments): (PathBuf, &[&str]) = match (registry.file_name(), registry.parent()) {
        (Some(name), Some(data_root)) if name == "palaces" => {
            (data_root.to_path_buf(), &LEGACY_SEGMENTS)
        }
        _ => (registry.clone(), &FLAT_SEGMENTS),
    };
    for segment in segments {
        root.push(segment);
        ensure_dir(&root, |path, reason| SnapshotError::BackupRootUnsafe {
            path,
            reason: reason.to_string(),
        })?;
    }
    Ok(root)
}

/// Verify or create the parents of `root/name`; return `root/name`.
///
/// Why: the parents are user-named; each must be a real directory under the
/// root, never a symlink, and the final directory must not exist yet.
/// What: `name` must pass [`check_backup_name`]. A read-only pass `lstat`s
/// each existing parent: a symlink or non-directory is
/// [`SnapshotError::UnsafeBackupComponent`]; when every parent exists, an
/// existing final entry (a dangling symlink included) is
/// [`SnapshotError::DestinationExists`]. A create pass then `mkdir`s each
/// missing parent in order, through `hook` ([`SnapshotStep::CreateDest`]).
/// Last, the canonical parent must lie under the canonical `root`. On any
/// failure after the first create, the directories this call created are
/// removed, deepest first. The final directory is never created here; the
/// staging rename makes it.
/// Test: `a_symlinked_component_is_refused_and_writes_nothing`,
/// `a_name_segment_that_is_a_file_is_refused`,
/// `an_existing_destination_is_refused`,
/// `a_failed_parent_create_removes_what_it_created`,
/// `a_segment_swapped_after_the_walk_is_refused`,
/// `a_valid_nested_name_lands_under_the_backup_root`.
pub(crate) fn prepare_dest(root: &Path, name: &Path, hook: SnapshotHook) -> Result<PathBuf> {
    check_backup_name(name)?;
    let segments: Vec<&std::ffi::OsStr> = name.components().map(|c| c.as_os_str()).collect();
    let Some((last, parents)) = segments.split_last() else {
        return Err(SnapshotError::InvalidBackupName {
            name: name.to_path_buf(),
            reason: "it is empty".to_string(),
        });
    };
    let mut parent = root.to_path_buf();
    let mut existing = 0;
    for segment in parents {
        let next = parent.join(segment);
        match std::fs::symlink_metadata(&next) {
            Ok(meta) => {
                if let Some(reason) = unsafe_kind(&meta) {
                    return Err(SnapshotError::UnsafeBackupComponent {
                        path: next,
                        reason: reason.to_string(),
                    });
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => break,
            Err(e) => return Err(io_err("stat", &next)(e)),
        }
        parent = next;
        existing += 1;
    }
    if existing == parents.len() {
        let target = parent.join(last);
        match std::fs::symlink_metadata(&target) {
            Ok(_) => return Err(SnapshotError::DestinationExists { path: target }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err("stat", &target)(e)),
        }
    }
    let mut created = Vec::new();
    let result = create_and_contain(root, parent, &parents[existing..], hook, &mut created);
    if result.is_err() {
        remove_created(&created);
    }
    result.map(|parent| parent.join(last))
}

/// The create pass and containment check of [`prepare_dest`].
fn create_and_contain(
    root: &Path,
    mut parent: PathBuf,
    missing: &[&std::ffi::OsStr],
    hook: SnapshotHook,
    created: &mut Vec<PathBuf>,
) -> Result<PathBuf> {
    for segment in missing {
        parent.push(segment);
        hook(SnapshotStep::CreateDest, &parent)
            .and_then(|()| std::fs::create_dir(&parent))
            .map_err(io_err("create", &parent))?;
        created.push(parent.clone());
    }
    // #9282 (ruling D): a tripwire for a segment swapped after the walk.
    let real = parent.canonicalize().map_err(io_err("resolve", &parent))?;
    if !real.starts_with(root) {
        return Err(SnapshotError::UnsafeBackupComponent {
            reason: format!(
                "it resolves to {}, outside the backup root {}",
                real.display(),
                root.display()
            ),
            path: parent,
        });
    }
    Ok(parent)
}

/// `lstat` `path`; create it when missing. `unsafe_err` builds the refusal
/// for a symlink or a non-directory.
fn ensure_dir(path: &Path, unsafe_err: fn(PathBuf, &str) -> SnapshotError) -> Result<()> {
    for _ in 0..RACE_RETRIES {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => {
                return match unsafe_kind(&meta) {
                    Some(reason) => Err(unsafe_err(path.to_path_buf(), reason)),
                    None => Ok(()),
                };
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => match std::fs::create_dir(path) {
                Ok(()) => return Ok(()),
                // Raced with another create: look again.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io_err("create", path)(e)),
            },
            Err(e) => return Err(io_err("stat", path)(e)),
        }
    }
    Err(unsafe_err(
        path.to_path_buf(),
        "it kept appearing and vanishing while it was created",
    ))
}

/// Why an `lstat` result is not a plain directory, or `None` when it is.
fn unsafe_kind(meta: &std::fs::Metadata) -> Option<&'static str> {
    if meta.file_type().is_symlink() {
        Some("it is a symlink")
    } else if !meta.is_dir() {
        Some("it is not a directory")
    } else {
        None
    }
}

/// Remove the directories a failed [`prepare_dest`] created, deepest first.
fn remove_created(created: &[PathBuf]) {
    for dir in created.iter().rev() {
        if let Err(e) = std::fs::remove_dir(dir) {
            tracing::warn!(path = %dir.display(),
                "#9282: could not remove a directory a failed backup created: {e}");
        }
    }
}
